import shutil,subprocess,pathlib,sys,os
R=pathlib.Path("/home/btgrant/Projects/tallow/firmware/mesh/core"); W=pathlib.Path(sys.argv[1])/"core"
M=[("src/wire.rs",'|| data[0] != VERSION {',' {',"open ignores version"),
("src/wire.rs",'let kind = FrameKind::from_u8(data[1])?;','let kind = FrameKind::from_u8(data[1]).unwrap_or(FrameKind::Beacon);',"unknown kind becomes Beacon"),
("src/wire.rs",'ble_window_ms: half(2),\n            ble_interval_ms: half(4),','ble_window_ms: half(4),\n            ble_interval_ms: half(2),',"decode swaps window/interval"),
("src/wire.rs",'probe_hz: data[9],\n            probe_size: data[10],','probe_hz: data[10],\n            probe_size: data[9],',"decode swaps probe fields"),
("src/wire.rs",'inquiry_period_s: half(6),','inquiry_period_s: u16::from_be_bytes([data[6],data[7]]),',"period big-endian"),
("src/wire.rs",'if data.len() != Self::WIRE {','if data.len() < Self::WIRE {',"long command accepted"),
("src/wire.rs",'if payload.len() > MAX_PAYLOAD {','if payload.len() >= MAX_PAYLOAD {',"218 refused"),
("src/wire.rs",'pub const MAX_FRAME: usize = 250;','pub const MAX_FRAME: usize = 255;',"frame 255"),
("src/mesh.rs",'payload[1..5].copy_from_slice(&parent.to_le_bytes());','payload[1..5].copy_from_slice(&parent.to_be_bytes());',"beacon parent BE"),
("src/mesh.rs",'payload[5..7].copy_from_slice(&self.beacon_no.to_le_bytes());','payload[7..9].copy_from_slice(&self.beacon_no.to_le_bytes());',"beacon number moved"),
("src/mesh.rs",'let head = self.head(FrameKind::Beacon, 1);','let head = self.head(FrameKind::Beacon, 16);',"beacon ttl 16"),
("src/names.rs",'record[7..9].copy_from_slice(&entry.company.to_le_bytes());','record[7..9].copy_from_slice(&entry.company.to_be_bytes());',"company BE"),
("src/names.rs",'record[6] = entry.kind;','record[6] = 0;',"name kind zero"),
("src/sightings.rs",'record[6] = entry.kind;','record[6] = 0;',"report kind zero"),
("src/sightings.rs",'record[9] = age.min(255) as u8;','record[9] = age.min(254) as u8;',"age cap 254"),
("src/sightings.rs",'(now_ms.wrapping_sub(entry.last_ms) as i32).max(0) as u32 / 1000','now_ms.wrapping_sub(entry.last_ms) / 1000',"negative age back"),
("src/mesh.rs",'        if frame.kind == FrameKind::Beacon && id_of(&src) != frame.origin {\n            self.rx_bad = self.rx_bad.wrapping_add(1);\n            return;\n        }\n','',"no beacon source check"),
("src/mesh.rs",'                    let before = self.parent().map(|parent| parent.id);\n                    if let Some(index) = self.parent {','                    let before = None;\n                    if let Some(index) = self.parent {',"silent ack loss"),
]
env=dict(os.environ,CARGO_HOME=str(W.parent/"ch"),CARGO_NET_OFFLINE="true")
shutil.copytree("/home/btgrant/Projects/tallow/firmware/mesh/.cache/registry",W.parent/"ch/registry")
miss=[]
for f,a,b,n in M:
    if W.exists(): shutil.rmtree(W)
    shutil.copytree(R,W,ignore=shutil.ignore_patterns("target"))
    t=(W/f).read_text(); assert a in t,n
    (W/f).write_text(t.replace(a,b))
    r=subprocess.run(["cargo","test","--offline","--target-dir",str(W.parent/"tgt")],cwd=W,capture_output=True,text=True,env=env,timeout=300)
    o=r.stdout+r.stderr
    tag="CAUGHT" if r.returncode else "MISSED"
    if "error[" in o or "could not compile" in o: tag="COMPILE"
    print(tag,n,flush=True)
    if r.returncode==0: miss.append(n)
print("missed",miss)
