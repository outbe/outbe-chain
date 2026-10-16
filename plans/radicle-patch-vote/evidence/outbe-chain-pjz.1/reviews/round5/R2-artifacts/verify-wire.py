from pathlib import Path
import json
p=Path(__file__).resolve().parent
frames=[];transport=None;time=None
for line in (p/'production-wire.stdout.log').read_text().splitlines():
 if not line.startswith('WIRE_VECTOR '):continue
 row=dict(x.split('=',1) for x in line.split()[1:]);version=int(row['version']);f=bytes.fromhex(row['frame']);assert f[:4]==b'rad'+bytes([version]);assert f[4]==2
 width=1<<(f[5]>>6);size=int.from_bytes(f[5:5+width],'big')&((1<<(width*8-2))-1);b=f[5+width:];assert len(b)==size and b[:2]==b'\x00\x06'
 key=b[2:34];assert key==transport if transport else True;transport=key
 assert b[98:100]==b'\x00\x14' and b[100:120]==bytes([0x11])*20 and b[120:122]==b'\x00\x01'
 key_size=32 if version==1 else (33 if b[122]==0 else 97)
 if version==2:assert b[122] in [0,1]
 if key_size==32:assert b[122:154]==transport
 if key_size==33:assert b[123:155]==transport
 at=122+key_size;assert b[at:at+2]==b'\x00\x14' and b[at+2:at+22]==bytes([0x22])*20
 timestamp=b[at+22:];assert len(timestamp)==8;assert timestamp==time if time else True;time=timestamp
 assert size==130+key_size+22
 frames.append(dict(version=version,key_size=key_size,refs_at_bytes=key_size+22,payload_bytes=size,frame_bytes=len(f),rid_length=20,oid_length=20))
assert [(f['version'],f['refs_at_bytes']) for f in frames]==[(1,54),(2,119),(2,55)]
assert (65535-130)//119==549
(p/'independent-wire.json').write_text(json.dumps(dict(result='PASS',frames=frames,max_full_bls_entries=549),indent=2)+'\n')
print('PASS independently parsed signed production frames: V1 Ed54, V2 BLS119, V2 Ed55; RID and at u16be20, unchanged transport and timestamp, max549 full BLS entries')
