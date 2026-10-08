import re,time,json
pattern = r'href="(/astral-sh/python-build-standalone/releases/download/\d{8}/([^"]+\.tar\.gz))"'
href='href="/astral-sh/python-build-standalone/releases/download/20260929/cpython-3.13.15+20260929-x86_64-pc-windows-msvc-install_only.tar.gz"'
rows=[]
for size in (20,40,80):
    for kind in ("ascii","unicode","bytes"):
        subject=((" "*2000)+href+("雪" if kind=="unicode" else ""))*size
        p=pattern
        if kind=="bytes":
            subject=subject.encode();p=p.encode()
        rx=re.compile(p)
        start=time.perf_counter();matches=list(rx.finditer(subject));iterate=time.perf_counter()-start
        start=time.perf_counter();groups=[(m.group(1),m.group(2)) for m in matches];extract=time.perf_counter()-start
        start=time.perf_counter();found=rx.findall(subject);findall=time.perf_counter()-start
        assert len(matches)==size and groups==found
        rows.append({"size":size,"kind":kind,"length":len(subject),"iterate":iterate,"extract":extract,"findall":findall})
print(json.dumps(rows))
