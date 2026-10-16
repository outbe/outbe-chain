"""Reviewer's independent offset/length check over actual production signed frames."""
import re,hashlib
from pathlib import Path
out=Path(__file__).resolve().parent
rows=re.findall(r'WIRE_VECTOR version=(\d) key=(\S+) frame=([0-9a-f]+)',(out/'production-wire.log').read_text())
assert len(rows)==3
lengths=[];transport=None
for v,key,raw in rows:
 b=bytes.fromhex(raw);assert b[:4]==b'rad'+bytes([int(v)]) and b[4]==2
 length_size=1<<(b[5]>>6);n=int.from_bytes(b[5:5+length_size],'big')&((1<<(8*length_size-2))-1)
 body=b[5+length_size:];assert len(body)==n;assert body[:2]==b'\0\x06'
 signer=body[2:34];assert len(body[34:98])==64
 if transport is None:transport=signer
 assert signer==transport
 assert body[98:100]==b'\0\x14' and body[100:120]==bytes.fromhex('11'*20)
 assert body[120:122]==b'\0\x01'
 prefix=122
 if int(v)==1:klen=32;assert body[prefix:prefix+32]==transport
 elif len(lengths)==1:klen=97;assert body[prefix]==1
 else:klen=33;assert body[prefix]==0 and body[prefix+1:prefix+33]==transport
 assert body[prefix+klen:prefix+klen+2]==b'\0\x14'
 assert body[prefix+klen+2:prefix+klen+22]==bytes.fromhex('22'*20)
 assert int.from_bytes(body[-8:],'big')==1234567
 refs_at=klen+22;assert len(body)==130+refs_at
 lengths.append(refs_at);print(f'PASS wire v{v} RefsAt={refs_at} body={len(body)} fixed_overhead=130 frame_sha256={hashlib.sha256(b).hexdigest()}')
assert lengths==[54,119,55]
assert (65535-130)//119==549 and 130+550*119>65535
print('PASS legacy/V2 RID and at u16be(20) prefixes, unchanged Ed25519 transport, 549 BLS RefsAt payload bound')
