import math, random
from PIL import Image, ImageDraw, ImageFont

W,H,S=1300,300,2
ALPHA=2.87e-7; QSTAR=0.33
NB=90; X0,X1=40,1260; STEP=(X1-X0)/NB
HY,CBY,BY,BH=100,124,190,28
SECW=3.2*STEP
P1=50; PER=4; HOLD=100

def font(sz,b=False):
    for p in ("/System/Library/Fonts/Helvetica.ttc","/System/Library/Fonts/HelveticaNeue.ttc","/Library/Fonts/Arial.ttf"):
        try: return ImageFont.truetype(p,sz*S)
        except Exception: pass
    return ImageFont.load_default()
def cdf(n,d,p):
    if p<=0: return 1.0
    if p>=1: return 0.0
    return sum(math.exp(math.lgamma(n+1)-math.lgamma(k+1)-math.lgamma(n-k+1)+k*math.log(p)+(n-k)*math.log1p(-p)) for k in range(d+1))
def upper(n,d):
    if n==0 or d>=n: return 1.0
    lo,hi=1e-9,1.0
    for _ in range(50):
        mid=(lo+hi)/2
        if cdf(n,d,mid)>ALPHA: lo=mid
        else: hi=mid
    return (lo+hi)/2
def cub(p0,p1,p2,p3,n=24):
    out=[]
    for t in range(n+1):
        u=t/n
        out.append(((1-u)**3*p0[0]+3*(1-u)**2*u*p1[0]+3*(1-u)*u*u*p2[0]+u**3*p3[0],
                    (1-u)**3*p0[1]+3*(1-u)**2*u*p1[1]+3*(1-u)*u*u*p2[1]+u**3*p3[1]))
    return out

rng=random.Random(29)
tagged=sorted(rng.sample(range(NB),60))
strong=set(sorted(rng.sample(tagged,32)))
diss={80}
cpv=[i for i in tagged if i not in strong and i not in diss]
order=list(range(NB-1,-1,-1))
tags_t=[0]; diss_t=[0]
for i in order:
    tags_t.append(tags_t[-1]+(1 if i in tagged else 0))
    diss_t.append(diss_t[-1]+(1 if i in diss else 0))
BOUND=[upper(tags_t[t],diss_t[t]) for t in range(NB+1)]
SUCCESS=next(t for t in range(NB+1) if BOUND[t]<=QSTAR)

def first(pred):
    for t,i in enumerate(order):
        if pred(i): return t
    return 0
T_TAG=first(lambda i:i in tagged)
T_STR=first(lambda i:i in strong)
T_CPV=first(lambda i:i in cpv)
T_DIS=first(lambda i:i in diss)

pauses={}
seq=[]
for t in range(0,SUCCESS+1):
    seq += [t]*PER
    if t in pauses: seq += [t]*pauses[t]
seq += [SUCCESS]*HOLD
seq = [0]*P1 + seq
TOTAL=len(seq)
pause_frames={t:[] for t in pauses}
for fi,tv in enumerate(seq):
    if tv in pauses: pause_frames[tv].append(fi)
pause_pos={fi:i for fi in range(TOTAL) for i in [0]}
for tv,lst in pause_frames.items():
    for i,fi in enumerate(lst): pause_pos[fi]=i
CAP={T_TAG:"A tag says which RSK header this block saw.",
     T_STR:"Strong: the 20-byte prefix names that header outright.",
     T_CPV:"Soft: no header. Seven checkpoints match instead.",
     T_DIS:"It matches nothing. That counts against the chain.",
     SUCCESS:"Under the 67% line. Everything it checked is final."}

# gradient strip
sw=max(1,int(7*SECW*S)); sh=BH*S
strip=Image.new("RGBA",(sw,sh),(0,0,0,0)); sd=ImageDraw.Draw(strip)
for sx in range(sw):
    t=sx/(sw-1); a=0.05+0.20*abs(t-0.5)*2
    sd.rectangle((sx,0,sx,sh),fill=(110,110,110,int(a*255)))

def T(dr,x,y,s,sz=10,col="#141414",b=False,anchor="la"):
    dr.text((x*S,y*S),s,font=font(sz,b),fill=col,anchor=anchor)
def L(dr,x1,y1,x2,y2,col="#CCC",w=1):
    dr.line((x1*S,y1*S,x2*S,y2*S),fill=col,width=int(w*S))
def R(dr,x,y,w,h,fill,outline=None,ow=1):
    dr.rectangle((x*S,y*S,(x+w)*S,(y+h)*S),fill=fill,outline=outline,width=int(ow*S))
def C(dr,x,y,r,fill,outline="white",ow=1):
    dr.ellipse((x*S-r*S,y*S-r*S,x*S+r*S,y*S+r*S),fill=fill,outline=outline,width=int(ow*S))

def ribbon(img,dr,i,alpha=1.0,cur=False):
    x=X0+i*STEP+6
    k=int((x-X0)//SECW); x1=X0+k*SECW; x0=max(X0+2,x1-7*SECW)
    a,b=x-3,x+3
    mid=(CBY+12+BY)/2
    p1=cub((a,CBY+12),(a,mid+8),(x0,mid-8),(x0,BY))
    p2=cub((x1,BY),(x1,mid-8),(b,mid+8),(b,CBY+12))
    c=strip.crop((0,0,max(1,int((x1-x0)*S)),sh))
    img.paste(c,(int(x0*S),int(BY*S)),c)
    ov=Image.new("RGBA",(W*S,H*S),(0,0,0,0)); od=ImageDraw.Draw(ov)
    od.polygon([(p[0]*S,p[1]*S) for p in p1+[(x0,BY+BH),(x1,BY+BH)]+p2],fill=(110,110,110,int(22*alpha)))
    img=Image.alpha_composite(img.convert("RGBA"),ov).convert("RGB")
    dr=ImageDraw.Draw(img)
    dr.line([(p[0]*S,p[1]*S) for p in p1],fill=(110,110,110,int(150*alpha)),width=int(0.7*S))
    dr.line([(p[0]*S,p[1]*S) for p in p2],fill=(110,110,110,int(150*alpha)),width=int(0.7*S))
    return img

def nar(t):
    if t<T_TAG: return "Start at the tip. Read backwards."
    if t<T_STR: return "A tag says which RSK header this block saw."
    if t<T_CPV: return "Strong: the 20-byte prefix names that header outright."
    if t<T_DIS: return "Soft: no header. Seven checkpoints match instead \u2014 448 headers back."
    if t<SUCCESS: return "Each clean tag brings the bound down. One dissent costs."
    return "Under q*. Everything it checked is final."

# ruler
RY=252; RX0,RX1=40,1260
def rx(v): return RX0+v*(RX1-RX0)

for f in range(TOTAL):
    t=seq[f]
    img=Image.new("RGB",(W*S,H*S),"white"); dr=ImageDraw.Draw(img)
    T(dr,40,22,"RSK: Fork-Aware Ephemeral Sidechains",24,"#141414",True)
    T(dr,40,66,"A FINAL block is endorsed by 67% merge-mining tags with statistical confidence",13,"#141414")

    # row
    for i in range(NB):
        x=X0+i*STEP
        if f>=P1 or i< int((f+1)/P1*NB):
            R(dr,x+1,HY,10,16,"#FFFFFF","#000000",1); L(dr,x+2,HY+5,x+9,HY+5,"#000000",1)
        else:
            R(dr,x+1,HY,10,16,"#FFFFFF","#E3E3E3",1)
    R(dr,X0,BY,X1-X0,BH,"#FAFAFA","#6E6E6E",1)
    for k in range(0,int((X1-X0)/SECW)+2):
        xx=X0+k*SECW
        if xx<X1: L(dr,xx,BY,xx,BY+BH,"#E6E6E6",0.6)

    done=min(t,SUCCESS)
    for j in range(done):
        i=order[j]; x=X0+i*STEP
        if i in tagged:
            if i in diss:
                R(dr,x+2,CBY,8,12,"#F7DEDE","#C0392B",1.3); C(dr,x+6,CBY+6,2.2,"#C0392B")
            else:
                R(dr,x+2,CBY,8,12,"#FFFFFF","#000000",1); C(dr,x+6,CBY+6,2.2,"#2E7D32")
                if i in strong:
                    L(dr,x+6,CBY+12,x-2,BY,"#000000",1.0)
                    R(dr,x-3,BY,2.2,BH,"#000000",None,0)
                else:
                    img=ribbon(img,dr,i,0.6); dr=ImageDraw.Draw(img)
        else:
            R(dr,x+2,CBY,8,12,"#F0F0F0","#BDBDBD",1)

    cap=None
    if seq[f] in pause_frames and pause_pos[f]>=6:
        cap=CAP.get(t)
    if cap:
        T(dr,40,BY+BH+34,cap,12,"#141414",True)
    if t>=SUCCESS:
        R(dr,X0+1,BY,2.4,BH,"#000000",None,0)
        T(dr,X0+7,BY-8,"FINAL",10.5,"#141414",True)

    n,d=tags_t[t],diss_t[t]
    low=max(0.0,1.0-BOUND[t]); obs=((n-d)/n) if n else 0.0
    RY=252; RX0,RX1=40,1260
    def rx(v): return RX0+v*(RX1-RX0)
    R(dr,RX0,RY-10,RX1-RX0,20,"#D9D9D9",None,0)
    R(dr,RX0,RY-10,rx(low)-RX0,20,"#141414",None,0)
    if d>0 and n>0:
        w=(RX1-RX0)*(d/n)
        R(dr,RX1-w,RY-10,w,20,"#C0392B",None,0)
    R(dr,RX0,RY-10,RX1-RX0,20,None,"#141414",1)
    if rx(obs)-rx(low)>100:
        T(dr,(rx(low)+rx(obs))/2,RY+3,"error margin",9,"#555555",True,"mm")
    L(dr,rx(0.67),RY-26,rx(0.67),RY+26,"#141414",2)
    T(dr,rx(0.67),RY+18,"67%",10.5,"#141414",True,"mm")
    T(dr,RX0,RY+18,"0",9,"#9A9A9A")
    T(dr,RX1,RY+18,"100%",9,"#9A9A9A",False,"ra")


    img.resize((W,H),Image.LANCZOS).save(f"/tmp/scanframes/f_{f:04d}.png")

print("frames",TOTAL,"success",SUCCESS,"t_tag",T_TAG,"t_str",T_STR,"t_cpv",T_CPV,"t_dis",T_DIS)

# assemble the GIF from the rendered frames
import glob
from pathlib import Path as _P
_files = sorted(glob.glob("/tmp/scanframes/f_*.png"))
if _files:
    from PIL import Image as _I
    _base = _I.open(_files[-1]).convert("RGB")
    _pal = _base.quantize(colors=256, method=_I.MEDIANCUT)
    _frames = [_I.open(f).convert("RGB").quantize(palette=_pal, dither=_I.NONE) for f in _files]
    _out = _P(__file__).resolve().parents[2] / "finality-scan.gif"
    _frames[0].save(_out, save_all=True, append_images=_frames[1:], duration=40, loop=0, optimize=False)
    print("wrote", _out)
