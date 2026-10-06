#!/usr/bin/env python3
"""Local four-validator snapshot workflow, optionally with a separate FullNode donor.
No SGX hardware claim. Fixtures use the ordinary authenticated enclave transport.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import time

from enclave_upgrade_e2e import Network, ROOT

class SnapshotNetwork(Network):
    def exported_args(self, i):
        directory = self.node_dir(i)
        return ['--chain', self.network/'genesis.json', '--datadir', directory/'data',
                '--projection.storage-config', directory/'offchain-storage.toml']

    def wait_recipient(self, target, label, i=4, process='snapshot-fullnode'):
        end = time.monotonic()+180
        while time.monotonic() < end:
            if self.processes[process].poll() is not None:
                raise RuntimeError(f'{process} exited; see {process}.log')
            try:
                block = self.rpc('eth_getBlockByNumber', ['finalized',False], i)
                height = int(block['number'],16)
                if height >= target:
                    donor = self.rpc('eth_getBlockByNumber',[hex(height),False],0)
                    assert donor['hash']==block['hash'] and donor['stateRoot']==block['stateRoot']
                    self.report['steps'].append({'label':label,'height':height,'hash':block['hash'],'state_root':block['stateRoot']})
                    print(f'snapshot: {label}: finalized {height}',flush=True)
                    return height
            except (OSError,RuntimeError,TypeError):
                pass
            time.sleep(0.3)
        raise TimeoutError('snapshot FullNode did not catch up')

    def fullnode_args(self, bundle, key, i=4):
        base = json.loads((bundle/'node-args.json').read_text())
        return [self.args.new_node,'node',*base,'--upstream',f'http://127.0.0.1:{self.port("rpc",0)}',
            '--p2p-secret-key',key,'--http','--http.addr','127.0.0.1','--http.port',self.port('rpc',i),
            '--http.api','eth,net,web3,outbe','--disable-discovery','--addr','127.0.0.1',
            '--port',self.port('p2p',i),'--authrpc.port',self.port('auth',i),
            '--consensus.listen-addr',f'127.0.0.1:{self.port("consensus",i)}','--consensus.use-local-defaults',
            '--tee-enclave-socket',f'127.0.0.1:{self.port("enclave",i)}','--tee-session-mode','production-node-host',
            '--engine.persistence-threshold','0','--engine.memory-block-buffer-target','0',
            '--ipcpath',bundle/'reth.ipc','--log.file.directory',bundle/'logs']

    def start_fresh_donor_fullnode(self):
        """Join with a fresh identity and sync normally before becoming a donor."""
        i = 5
        bundle = self.node_dir(i)
        bundle.mkdir(parents=True, mode=0o700)
        (bundle/'offchain-storage.toml').write_text(
            (self.node_dir(0)/'offchain-storage.toml').read_text())
        (bundle/'node-args.json').write_text(json.dumps(list(map(str,self.exported_args(i)))))
        key = bundle/'reth-p2p-secret.hex'
        key.write_text(secrets.token_hex(32)); key.chmod(0o600)
        wallet = bundle/'snapshot-signing-key.hex'
        wallet.write_text(secrets.token_hex(32)); wallet.chmod(0o600)
        address = subprocess.check_output(['cast','wallet','address','--private-key',wallet.read_text()],text=True).strip()
        self.start_enclave(i)
        time.sleep(1)
        self.run(['cast','send','--rpc-url',f'http://127.0.0.1:{self.port("rpc",0)}',
            '--private-key',(self.node_dir(0)/'evm-key.hex').read_text().strip(),
            address,'--value','1ether'],'fund-snapshot-donor')
        expiry = int(self.rpc('eth_getBlockByNumber',['finalized',False])['timestamp'],16)+7200
        self.run([self.args.cli,'--rpc-url',f'http://127.0.0.1:{self.port("rpc",0)}',
            '--private-key',wallet.read_text(),'tee','join',
            '--enclave-socket',f'127.0.0.1:{self.port("enclave",i)}',
            '--node-data-dir',bundle/'data','--reth-p2p-secret-key',key,
            '--genesis',self.network/'genesis.json','--binding-id',secrets.token_hex(32),
            '--valid-until',expiry,'--timeout-secs','180'],'snapshot-donor-join',timeout=240)
        self.checked_nodes.add('snapshot-donor')
        self.start('snapshot-donor',self.fullnode_args(bundle,key,i))
        target = int(self.rpc('eth_getBlockByNumber',['finalized',False])['number'],16)
        self.wait_recipient(target,'fresh-donor-fullnode-started',i,'snapshot-donor')
        self.report['donor_role'] = 'fullnode'
        return bundle, key, wallet

    def execute_snapshot(self):
        self.prepare()
        for i in range(4):
            self.start_enclave(i)
            self.start_sidecar(i)
            self.start_node(i,new=True)
        donor = None
        if self.args.donor_fullnode:
            self.wait_height(5,'before-donor-onboarding')
            donor = self.start_fresh_donor_fullnode()
        self.wait_height(self.args.cut_height,'before-native-snapshot',timeout=1200)
        if self.args.cut_height >= 340:
            for i in range(4):
                log = re.sub(r'\x1b\[[0-9;]*m','',(self.directory/f'node-{i}.log').read_text())
                assert any('VRF/DKG material activated' in line and 'activation_height=300 ' in line
                           for line in log.splitlines()), f'node {i} did not activate the new DKG material'
            self.report['steps'].append({'label':'dkg-rotation-verified','activation_height':300})
        if donor:
            self.wait_recipient(self.args.cut_height,'donor-fullnode-caught-up',5,'snapshot-donor')
        validators_before = {i:self.processes[f'node-{i}'].pid for i in range(4)}
        height_before = int(self.rpc('eth_getBlockByNumber',['finalized',False])['number'],16)
        self.stop('snapshot-donor' if donor else 'node-3')
        archive = self.directory/'snapshot.tar'
        output = self.run([self.args.new_node,'snapshot','export','--output',archive,
            '--signing-key',donor[2] if donor else self.node_dir(3)/'evm-key.hex',
            '--',*self.exported_args(5 if donor else 3)],'snapshot-export',timeout=600)
        signer = re.search(r'creator_public_key=([0-9a-f]+)',output)[1]
        cut = int(re.search(r'finalized_height=(\d+)',output)[1])
        if donor:
            self.wait_height(height_before+2,'validators-progress-with-donor-offline')
            for i,pid in validators_before.items():
                assert self.processes[f'node-{i}'].pid == pid
                assert self.processes[f'node-{i}'].poll() is None
            self.report['validators_preserved_during_export'] = validators_before
            self.start('snapshot-donor',self.fullnode_args(donor[0],donor[1],5))
            self.wait_recipient(cut+3,'donor-fullnode-restarted',5,'snapshot-donor')
        else:
            self.start_node(3,new=True)
        self.wait_height(cut+3,'donor-resumed')
        self.run([self.args.new_node,'snapshot','validate','--archive',archive,'--expected-signer',signer],'snapshot-validate',timeout=600)
        # Wrong signer and a damaged archive must never publish a partial node.
        key = self.directory/'recipient-p2p.hex'
        key.write_text(secrets.token_hex(32)); key.chmod(0o600)
        wrong = '0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798'
        for label, artifact, expected in [('wrong-signer',archive,wrong)]:
            target = self.directory/label
            result = subprocess.run(list(map(str,[self.args.new_node,'snapshot','import','--archive',artifact,'--chain',self.network/'genesis.json','--expected-signer',expected,'--into',target])),stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
            assert result.returncode != 0 and not target.exists()
            assert b'does not match expected public key' in result.stdout, result.stdout[-1000:]
            self.report['steps'].append({'label':label+'-rejected'})
        wrong_genesis = self.directory/'wrong-genesis.json'
        genesis = json.loads((self.network/'genesis.json').read_text())
        genesis['extraData'] = '0x1234'
        wrong_genesis.write_text(json.dumps(genesis))
        target = self.directory/'wrong-genesis-recipient'
        result = subprocess.run(list(map(str,[self.args.new_node,'snapshot','import','--archive',archive,'--chain',wrong_genesis,'--expected-signer',signer,'--into',target])),stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=60)
        assert result.returncode != 0 and not target.exists()
        assert (b'genesis/network mismatch' in result.stdout
                or b'TEE policy schedule does not match ChainSpec identity' in result.stdout), result.stdout[-1000:]
        assert not list(self.directory.glob('.rudis-snapshot-import-*'))
        self.report['steps'].append({'label':'wrong-genesis-rejected'})
        # Stream a modified byte without making a second full-size corrupt copy.
        with archive.open('r+b') as handle:
            handle.seek(-2048,os.SEEK_END); position=handle.tell(); original=handle.read(1)
            handle.seek(position); handle.write(bytes([original[0]^1]))
        bad_target=self.directory/'damaged'
        try:
            result=subprocess.run(list(map(str,[self.args.new_node,'snapshot','import','--archive',archive,'--chain',self.network/'genesis.json','--expected-signer',signer,'--into',bad_target])),stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=600)
            assert result.returncode != 0 and not bad_target.exists(),result.stdout[-1000:]
        finally:
            with archive.open('r+b') as handle:
                handle.seek(position);handle.write(original)
        self.report['steps'].append({'label':'damaged-archive-rejected'})
        bundle = self.directory/'recipient'
        self.run([self.args.new_node,'snapshot','import','--archive',archive,'--chain',self.network/'genesis.json','--expected-signer',signer,'--into',bundle],'snapshot-import',timeout=600)
        start_args=json.loads((bundle/'node-args.json').read_text())
        data=Path(start_args[start_args.index('--datadir')+1])
        # The custom --datadir used by Rudis is the resolved data directory.
        assert (data/'db/mdbx.dat').exists()
        for forbidden in ['tee-node-host-v1','keys','p2p_secret']:
            assert not (data/forbidden).exists(), forbidden
        assert not list(bundle.rglob('dkg_share.hex'))
        assert not list(bundle.rglob('ocomp-evm-key.hex'))
        # Both the signed archive and the placed native state must agree.
        audit_path = self.directory/'native-audit.json'
        with (self.directory/'snapshot-native-validate.log').open('wb') as log:
            validation = subprocess.run(list(map(str,[self.args.new_node,'snapshot','validate',
                '--archive',archive,'--expected-signer',signer,'--checks','all',
                '--report',audit_path,'--',*start_args])), stdout=log,stderr=subprocess.STDOUT,timeout=600)
        audit = json.loads(audit_path.read_text())
        for name in ('files','provenance','headers','evm','ce','ocomp'):
            assert audit['checks'][name]['status']=='passed', (name,audit)
        body_status = audit['checks']['bodies']['status']
        if body_status == 'incomplete':
            p,q,e = (audit['observed'][name] for name in ('p','q','e'))
            assert p['number'] < q['number'] <= e['number'], audit
            assert audit['body_structure']=={'checkpoint':p,'status':'passed'}, audit
            assert validation.returncode != 0, 'incomplete equality must remain visible to operators'
        else:
            assert body_status == 'passed' and validation.returncode == 0, audit
        assert not audit['required_missing'], audit
        imported_audit = json.loads((bundle/'snapshot-validation.json').read_text())
        assert imported_audit['observed']==audit['observed'], (imported_audit,audit)
        self.report['native_audit']=audit
        self.node_dir(4).mkdir(parents=True,exist_ok=True)
        self.start_enclave(4)
        time.sleep(1)
        expiry=int(self.rpc('eth_getBlockByNumber',['finalized',False])['timestamp'],16)+7200
        # Fund a fresh recipient wallet. The test reuses neither validator association
        # nor donor identity.
        private=secrets.token_hex(32)
        address=subprocess.check_output(['cast','wallet','address','--private-key',private],text=True).strip()
        funder=(self.node_dir(0)/'evm-key.hex').read_text().strip()
        self.run(['cast','send','--rpc-url',f'http://127.0.0.1:{self.port("rpc",0)}','--private-key',funder,address,'--value','1ether'],'fund-snapshot-recipient')
        self.run([self.args.cli,'--rpc-url',f'http://127.0.0.1:{self.port("rpc",0)}','--private-key',private,
            'tee','join','--enclave-socket',f'127.0.0.1:{self.port("enclave",4)}','--node-data-dir',data,
            '--reth-p2p-secret-key',key,'--genesis',self.network/'genesis.json','--binding-id',secrets.token_hex(32),
            '--valid-until',expiry,'--timeout-secs','180'],'snapshot-recipient-join',timeout=240)
        target=int(self.rpc('eth_blockNumber'),16)
        self.checked_nodes.add('snapshot-fullnode')
        self.start('snapshot-fullnode',self.fullnode_args(bundle,key))
        reached=self.wait_recipient(target+3,'imported-fullnode-caught-up')
        log=re.sub(r'\x1b\[[0-9;]*m','',(self.directory/'snapshot-fullnode.log').read_text())
        barrier=[line for line in log.splitlines() if 'certified follower startup recovery barrier completed' in line]
        assert barrier, 'missing imported recovery checkpoint evidence'
        recovery=int(re.search(r'recovery_height=(\d+)',barrier[0])[1])
        assert recovery >= cut and recovery > 0, barrier[0]
        executed=[int(re.search(r'height=(\d+)',line)[1]) for line in log.splitlines() if 'finalized block accepted by execution layer' in line]
        assert executed and min(executed) == recovery+1, executed[:10]
        self.report['recovery_height']=recovery
        self.report['first_executed_height']=min(executed)
        # Runtime startup selects the imported checkpoint, not height zero.
        self.report['imported_height']=cut
        self.report['first_start_log']=str(self.directory/'snapshot-fullnode.log')
        self.stop('snapshot-fullnode')
        self.start('snapshot-fullnode',self.fullnode_args(bundle,key))
        self.wait_recipient(reached+4,'imported-fullnode-restarted')
        self.wait_height(reached+4,'network-preserved')
        assert hashlib.sha256((self.network/'genesis.json').read_bytes()).hexdigest()==self.genesis_digest
        self.report['result']='passed'

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.set_defaults(cross_epoch=False, epoch_length_blocks=300, inject_proof_faults=False)
    for name in ('old-node','new-node','cli','keygen','enclave','radicle','output'):
        parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--port-offset',type=int,default=3500)
    parser.add_argument('--cut-height',type=int,default=340)
    parser.add_argument('--donor-fullnode',action='store_true',help='Export from a separately joined FullNode; keep all four validators running')
    args=parser.parse_args()
    for name in ('old_node','new_node','cli','keygen','enclave','radicle','output'):
        setattr(args,name,getattr(args,name).resolve())
    if args.output.exists(): parser.error('use a new output directory')
    network=SnapshotNetwork(args,'native-snapshot')
    try:
        network.execute_snapshot()
    except BaseException as error:
        network.report.update(result='failed',error=str(error));raise
    finally:
        try:
            network.close()
        except BaseException as error:
            network.report.update(result='failed',cleanup_error=str(error))
            raise
        finally:
            (network.directory/'result.json').write_text(json.dumps(network.report,indent=2)+'\n')
    print('native snapshot: PASSED',flush=True)
if __name__=='__main__':main()
