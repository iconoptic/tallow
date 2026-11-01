import shutil, subprocess, sys, pathlib
REPO = pathlib.Path("/home/btgrant/Projects/tallow")
S = pathlib.Path(sys.argv[1])
M = [
 ("cookie presence only","app/agents.py","if cookie and hmac.compare_digest(cookie.encode(), browser_token.encode()):","if cookie:"),
 ("cookie prefix","app/agents.py","if cookie and hmac.compare_digest(cookie.encode(), browser_token.encode()):","if cookie and browser_token.startswith(cookie):"),
 ("any bearer","app/agents.py","    agent = match_agent(agents, secret)\n    if agent is None:\n        return None\n","    agent = agents[-1]\n"),
 ("no scope check","app/h7020_panel.py",'        if scope not in scopes:\n            raise HTTPException(status_code=403, detail="missing scope")\n',''),
 ("no audit","app/h7020_panel.py",'            catalog.audit(actor, request.method, request.url.path)\n','            pass\n'),
 ("no rate limit","app/h7020_panel.py",'        if not limiter.allow():\n            raise HTTPException(status_code=429, detail="slow down")\n',''),
 ("socket left open","app/h7020_panel.py",'                sock_path.chmod(0o600)\n','                pass\n'),
 ("no cam address guard","app/cams.py",'        if address not in IOT_NET or address in (IOT_NET.network_address, IOT_NET.broadcast_address, AP_ADDR):\n            raise ValueError(f"{where}: {cam_id} must be a node on {IOT_NET}")\n',''),
 ("no title escape","app/h7020_panel.py",'source.replace("__CAMS_TITLE__", html.escape(book.title()))','source.replace("__CAMS_TITLE__", book.title())'),
 ("no net fill","app/h7020_panel.py",'''.replace(
            "__IOT_NET__", html.escape(str(camlib.IOT_NET))
        )''',''),
 ("login prefix","app/h7020_panel.py",'if not hmac.compare_digest(password_form.encode(), password.encode()):','if not password.startswith(password_form):'),
 ("sequence never plays","app/show.py",'        self._task = asyncio.create_task(self._run_sequence(pattern, generation))\n',''),
 ("scheduler off","app/h7020_panel.py",'        stage.start()\n',''),
 ("cam name renamed","app/h7020_panel.py",'{"cams": [{"id": cam.id, "name": cam.name} for cam in listed], "error": book.error}','{"cams": [{"id": cam.id, "label": cam.name} for cam in listed], "error": book.error}'),
 ("path back in the error","app/cams.py",'    title, cams = parse_inventory(raw)\n','    title, cams = parse_inventory(raw, path)\n'),
 ("utf-8 500","app/h7020_panel.py",'except (json.JSONDecodeError, UnicodeDecodeError) as exc:','except json.JSONDecodeError as exc:'),
 ("non-ascii cookie 500","app/agents.py","hmac.compare_digest(cookie.encode(), browser_token.encode())","hmac.compare_digest(cookie, browser_token)"),
 ("bearer on LAN","app/agents.py",'        if authorization:\n            return None\n        if cookie and','        if authorization:\n            return ("x", frozenset(SCOPES))\n        if cookie and'),
 ("cookie on socket","app/agents.py",'    if cookie:\n        return None\n    if authorization is None:','    if authorization is None:'),
 ("brightness bound","app/h7020_panel.py",None,None),
]
missed=[]
for name,f,a,b in M:
    if a is None: continue
    w=S/"t"
    if w.exists(): shutil.rmtree(w)
    (w).mkdir()
    shutil.copytree(REPO/"app",w/"app",ignore=shutil.ignore_patterns("__pycache__"))
    shutil.copytree(REPO/"tools/tests",w/"tools/tests",ignore=shutil.ignore_patterns("__pycache__"))
    for x in REPO.glob("tools/*.py"): shutil.copy(x,w/"tools")
    (w/"references").symlink_to(REPO/"references")
    (w/".venv").symlink_to(REPO/".venv")
    t=(w/f).read_text()
    assert t.count(a)>=1,name
    (w/f).write_text(t.replace(a,b))
    r=subprocess.run([str(REPO/".venv/bin/python"),str(w/"tools/tests/panel_contract.py"),"--panel-command",f"{REPO/'.venv/bin/python'} {w/'app/h7020_panel.py'} --preview"],capture_output=True,text=True,timeout=120)
    first=[l for l in (r.stderr+r.stdout).splitlines() if l.startswith("FAIL") or "Traceback" in l or "exited" in l]
    print(f"{'CAUGHT' if r.returncode else 'MISSED':7} {name:26} {first[0][:70] if first else ''}",flush=True)
    if not r.returncode: missed.append(name)
print("missed:",missed)
