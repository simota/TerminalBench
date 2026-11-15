import json,glob,math,sys,itertools
d=sys.argv[1]
def mwu(a,b):
    c=sorted([(v,0) for v in a]+[(v,1) for v in b]); n=len(c); r=[0]*n; tie=0; i=0
    while i<n:
        j=i
        while j+1<n and c[j+1][0]==c[i][0]: j+=1
        for k in range(i,j+1): r[k]=(i+j)/2+1
        t=j-i+1
        if t>1: tie+=t**3-t
        i=j+1
    n1,n2=len(a),len(b); r1=sum(x for x,(v,g) in zip(r,c) if g==0); u=r1-n1*(n1+1)/2; mu=n1*n2/2
    s=math.sqrt(n1*n2/12*((n+1)-tie/(n*(n-1))))
    if s==0: return 1.0
    z=(u-mu-0.5)/s if u>mu else (u-mu+0.5)/s if u<mu else 0
    return min(1,max(0,2*(1-0.5*(1+math.erf(abs(z)/math.sqrt(2))))))
print("metric,shell,runs,median_min,median_max,spread_pct,two_sided_p<0.05 (of 66 unordered pairs),ordered pairs failing gate at 0%/2%/5%/10% (of 132)")
tot=[0,0]
for sc in ("cmd-overhead","shell-startup"):
  for sh in ("zsh","bash"):
    R=[[r for r in json.load(open(f))["results"] if r["shell"]==sh][0] for f in sorted(glob.glob(f"{d}/{sc}-*.json"))]
    meds=[r["median"] for r in R]
    sig=sum(1 for a,b in itertools.combinations(R,2) if mwu(a["samples"],b["samples"])<0.05); tot[0]+=sig; tot[1]+=66
    fails=[]
    for thr in (0,0.02,0.05,0.10):
        fails.append(sum(1 for a,b in itertools.permutations(R,2) if (b["median"]-a["median"])/a["median"]>thr and mwu(a["samples"],b["samples"])<0.05))
    print(f"{R[0]['metric']},{sh},{len(R)},{min(meds):.2f},{max(meds):.2f},{(max(meds)-min(meds))/min(meds)*100:.1f},{sig} ({sig/66*100:.1f}%),{'/'.join(map(str,fails))}")
print(f"overall two-sided false-positive rate: {tot[0]}/{tot[1]} = {tot[0]/tot[1]*100:.1f}% (nominal 5%)")
