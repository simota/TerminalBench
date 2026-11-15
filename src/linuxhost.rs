//! Linux host facts for the report's environment controls: machine model,
//! CPU, power source and low-power mode, read from /sys and /proc. Roots
//! are parameters so the rules can be tested against fixture trees.
//!
//! Nothing here guesses. Each derived fact records the basis it rests on,
//! and a fact with no sound basis stays unknown.

use std::path::Path;

#[derive(Debug, Default, PartialEq)]
pub struct LinuxHost {
    pub machine_model: Option<String>,
    pub cpu: Option<String>,
    pub power_source: Option<&'static str>,
    pub power_basis: Option<&'static str>,
    pub low_power_mode: Option<bool>,
    pub low_power_basis: Option<&'static str>,
    pub platform_profile: Option<String>,
    pub cpu_governor: Option<String>,
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    let value = value.lines().next()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Strings firmware vendors leave in DMI fields they never filled in. Two
/// unrelated machines can share them, so they identify nothing.
fn is_placeholder(value: &str) -> bool {
    const PLACEHOLDERS: &[&str] = &[
        "to be filled by o.e.m.",
        "system product name",
        "system manufacturer",
        "default string",
        "not applicable",
        "not specified",
        "o.e.m.",
        "none",
    ];
    PLACEHOLDERS.contains(&value.to_lowercase().as_str())
}

fn dmi_pair(dmi: &Path, vendor: &str, product: &str) -> Option<String> {
    let product = read_trimmed(&dmi.join(product)).filter(|v| !is_placeholder(v))?;
    match read_trimmed(&dmi.join(vendor)).filter(|v| !is_placeholder(v)) {
        Some(vendor) => Some(format!("{vendor} {product}")),
        None => Some(product),
    }
}

fn cpu_model(proc_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(proc_root.join("cpuinfo")).ok()?;
    text.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            matches!(key.trim(), "model name" | "Model" | "cpu model").then(|| value.trim())
        })
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Board or system identity plus the CPU model. On a self-built desktop the
/// board alone does not fix the CPU, and the control exists to stop
/// comparisons across different hardware.
fn machine_model(sys_root: &Path, cpu: Option<&str>) -> Option<String> {
    let dmi = sys_root.join("devices/virtual/dmi/id");
    let base = dmi_pair(&dmi, "sys_vendor", "product_name")
        .or_else(|| dmi_pair(&dmi, "board_vendor", "board_name"))
        .or_else(|| read_trimmed(&sys_root.join("firmware/devicetree/base/model")))?;
    Some(match cpu {
        Some(cpu) => format!("{base} / {cpu}"),
        None => base,
    })
}

struct Supplies {
    external_online: bool,
    battery_discharging: bool,
    battery_present: bool,
    battery_status_known: bool,
}

fn power_supplies(sys_root: &Path) -> Supplies {
    let mut s = Supplies {
        external_online: false,
        battery_discharging: false,
        battery_present: false,
        battery_status_known: false,
    };
    let Ok(entries) = std::fs::read_dir(sys_root.join("class/power_supply")) else {
        return s;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let dir = entry.path();
        let Some(kind) = read_trimmed(&dir.join("type")) else {
            continue;
        };
        if kind == "Mains" || kind.starts_with("USB") {
            if read_trimmed(&dir.join("online")).as_deref() == Some("1") {
                s.external_online = true;
            }
        } else if kind == "Battery" {
            // scope=Device is a peripheral (mouse, headset), not the system.
            if read_trimmed(&dir.join("scope")).as_deref() == Some("Device") {
                continue;
            }
            s.battery_present = true;
            match read_trimmed(&dir.join("status")).as_deref() {
                Some("Discharging") => {
                    s.battery_discharging = true;
                    s.battery_status_known = true;
                }
                Some("Charging" | "Full" | "Not charging") => s.battery_status_known = true,
                _ => {}
            }
        }
    }
    s
}

pub fn collect(sys_root: &Path, proc_root: &Path) -> LinuxHost {
    let cpu = cpu_model(proc_root);
    let supplies = power_supplies(sys_root);

    let (power_source, power_basis) = if supplies.external_online {
        (Some("ac"), Some("external-supply-online"))
    } else if supplies.battery_discharging {
        (Some("battery"), Some("battery-discharging"))
    } else if supplies.battery_present {
        if supplies.battery_status_known {
            (Some("ac"), Some("battery-not-discharging"))
        } else {
            (None, None)
        }
    } else {
        // No system battery: the machine cannot be running from one.
        (Some("ac"), Some("no-system-battery"))
    };

    let platform_profile = read_trimmed(&sys_root.join("firmware/acpi/platform_profile"));
    let (low_power_mode, low_power_basis) = match platform_profile.as_deref() {
        Some(profile) => (Some(profile == "low-power"), Some("platform-profile")),
        // No profile switch and no battery: there is no low-power mode to be in.
        None if !supplies.battery_present => (Some(false), Some("no-battery-no-platform-profile")),
        None => (None, None),
    };

    LinuxHost {
        machine_model: machine_model(sys_root, cpu.as_deref()),
        cpu,
        power_source,
        power_basis,
        low_power_mode,
        low_power_basis,
        platform_profile,
        cpu_governor: read_trimmed(
            &sys_root.join("devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Tree(PathBuf);
    impl Tree {
        fn new(tag: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("tbench-host-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("proc")).unwrap();
            std::fs::create_dir_all(root.join("sys")).unwrap();
            Tree(root)
        }
        fn put(&self, rel: &str, content: &str) -> &Tree {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
            self
        }
        fn host(&self) -> LinuxHost {
            collect(&self.0.join("sys"), &self.0.join("proc"))
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const CPUINFO: &str = "processor\t: 0\nmodel name\t: AMD Ryzen 7 3700X 8-Core Processor\n";

    #[test]
    fn desktop_without_battery_is_on_ac_and_not_in_low_power_mode() {
        let t = Tree::new("desktop");
        t.put("proc/cpuinfo", CPUINFO)
            .put(
                "sys/devices/virtual/dmi/id/sys_vendor",
                "Gigabyte Technology Co., Ltd.\n",
            )
            .put(
                "sys/devices/virtual/dmi/id/product_name",
                "B550 AORUS ELITE V2\n",
            )
            .put(
                "sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
                "performance\n",
            );
        let h = t.host();
        assert_eq!(
            h.machine_model.as_deref(),
            Some("Gigabyte Technology Co., Ltd. B550 AORUS ELITE V2 / AMD Ryzen 7 3700X 8-Core Processor")
        );
        assert_eq!(h.power_source, Some("ac"));
        assert_eq!(h.power_basis, Some("no-system-battery"));
        assert_eq!(h.low_power_mode, Some(false));
        assert_eq!(h.low_power_basis, Some("no-battery-no-platform-profile"));
        assert_eq!(h.cpu_governor.as_deref(), Some("performance"));
    }

    #[test]
    fn laptop_on_battery_in_low_power_profile() {
        let t = Tree::new("laptop");
        t.put("proc/cpuinfo", CPUINFO)
            .put("sys/class/power_supply/AC/type", "Mains\n")
            .put("sys/class/power_supply/AC/online", "0\n")
            .put("sys/class/power_supply/BAT0/type", "Battery\n")
            .put("sys/class/power_supply/BAT0/status", "Discharging\n")
            .put("sys/firmware/acpi/platform_profile", "low-power\n");
        let h = t.host();
        assert_eq!(h.power_source, Some("battery"));
        assert_eq!(h.low_power_mode, Some(true));
        assert_eq!(h.low_power_basis, Some("platform-profile"));
    }

    #[test]
    fn laptop_charging_over_usb_c_counts_as_ac() {
        let t = Tree::new("usbc");
        t.put("sys/class/power_supply/ucsi-source-psy-1/type", "USB\n")
            .put("sys/class/power_supply/ucsi-source-psy-1/online", "1\n")
            .put("sys/class/power_supply/BAT0/type", "Battery\n")
            .put("sys/class/power_supply/BAT0/status", "Charging\n")
            .put("sys/firmware/acpi/platform_profile", "balanced\n");
        let h = t.host();
        assert_eq!(h.power_source, Some("ac"));
        assert_eq!(h.low_power_mode, Some(false));
    }

    #[test]
    fn battery_without_profile_leaves_low_power_mode_unknown() {
        let t = Tree::new("noprofile");
        t.put("sys/class/power_supply/BAT0/type", "Battery\n")
            .put("sys/class/power_supply/BAT0/status", "Unknown\n");
        let h = t.host();
        assert_eq!(h.power_source, None);
        assert_eq!(h.low_power_mode, None);
    }

    #[test]
    fn peripheral_batteries_are_not_the_system_battery() {
        let t = Tree::new("mouse");
        t.put("sys/class/power_supply/hidpp_battery_0/type", "Battery\n")
            .put("sys/class/power_supply/hidpp_battery_0/scope", "Device\n")
            .put(
                "sys/class/power_supply/hidpp_battery_0/status",
                "Discharging\n",
            );
        let h = t.host();
        assert_eq!(h.power_source, Some("ac"));
        assert_eq!(h.power_basis, Some("no-system-battery"));
    }

    #[test]
    fn placeholder_dmi_strings_fall_back_to_the_board_then_to_nothing() {
        let t = Tree::new("placeholder");
        t.put(
            "sys/devices/virtual/dmi/id/sys_vendor",
            "System manufacturer\n",
        )
        .put(
            "sys/devices/virtual/dmi/id/product_name",
            "System Product Name\n",
        )
        .put(
            "sys/devices/virtual/dmi/id/board_vendor",
            "ASUSTeK COMPUTER INC.\n",
        )
        .put("sys/devices/virtual/dmi/id/board_name", "PRIME X570-P\n");
        assert_eq!(
            t.host().machine_model.as_deref(),
            Some("ASUSTeK COMPUTER INC. PRIME X570-P")
        );

        let t = Tree::new("placeholder2");
        t.put(
            "sys/devices/virtual/dmi/id/product_name",
            "To Be Filled By O.E.M.\n",
        );
        assert_eq!(t.host().machine_model, None);
    }

    #[test]
    fn device_tree_model_is_used_without_dmi() {
        let t = Tree::new("dt");
        t.put(
            "sys/firmware/devicetree/base/model",
            "Raspberry Pi 5 Model B Rev 1.0\0",
        );
        assert_eq!(
            t.host().machine_model.as_deref(),
            Some("Raspberry Pi 5 Model B Rev 1.0")
        );
    }
}
