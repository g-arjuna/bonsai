#!/usr/bin/env python3
"""Opt-in dedicated EOS/SRL lab acceptance. No router configuration writes.

The receiver must already be running with durable journals. Only this task's
lab is restarted. Synthetic BGP source frames are never injected as BMP.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time
import urllib.parse
import urllib.request

p=argparse.ArgumentParser()
p.add_argument('--lab-root',required=True)
p.add_argument('--journal-root',required=True)
p.add_argument('--source-url',required=True)
p.add_argument('--speaker',required=True)
p.add_argument('--evidence-root')
args=p.parse_args()
root=Path(args.lab_root).resolve();journal=Path(args.journal_root).resolve()
assert root.parent==Path('/tmp') and root.name.startswith('bonsai-bn006-live-')
assert journal.parent==Path('/tmp') and journal.name.startswith('bonsai-bn006-live-')
out=Path(args.evidence_root).resolve() if args.evidence_root else root
if out != root:
    assert out.parent == root
    out.mkdir(exist_ok=False)
eos=f'clab-{root.name}-eos'; source=f'clab-{root.name}-source'
BASE='http://127.0.0.1:19029/internal/routing/v1/'
TOKEN='bn006-lab-internal'
SOURCE=args.source_url
SPEAKER=args.speaker
VIEWS={'LocRib','AdjRibInPostPolicy'}
report={'scope':'EOS 4.36.0F observed post-policy and Loc-RIB views; IPv4 global and real BLUE RD',
        'lab':root.name,'phases':{},'no_bmp_fixture_injection':True,'no_router_cli_configuration_writes':True}
children=[]

for node in (eos, source):
    label=subprocess.run(['docker','inspect','--format','{{ index .Config.Labels "containerlab" }}',node],
                         capture_output=True,text=True,check=True).stdout.strip()
    assert label==root.name, f'refusing to operate on non-owned container {node}'

def request(url,body=None):
    req=urllib.request.Request(url,data=None if body is None else json.dumps(body).encode(),
        headers={'x-bonsai-routing-token':TOKEN,'Content-Type':'application/json'})
    with urllib.request.urlopen(req,timeout=10) as r:return json.load(r)

def api(path,**query):return request(BASE+path+'?'+urllib.parse.urlencode(query))
def routes(prefix,rd):return api('routes',speaker=SPEAKER,prefix=prefix,vrf_rd=rd)['routes']
def save(name,value):
    (out/(name+'.json')).write_text(json.dumps(value,indent=2)+'\n');return value

def wait(desc,check,seconds=120):
    stop=time.monotonic()+seconds;error=None
    while time.monotonic()<stop:
        try:
            result=check()
            if result:return result
        except (OSError,ValueError,KeyError) as exc:error=type(exc).__name__
        time.sleep(1)
    raise AssertionError(f'timeout: {desc}; last error={error}')

def cmd(argv,log=None,timeout=600):
    r=subprocess.run(argv,capture_output=True,text=True,timeout=timeout)
    if log:(out/log).write_text(r.stdout+r.stderr)
    assert r.returncode==0,(argv,r.returncode,r.stderr[-1000:])
    return r.stdout

def start_source():
    try:
        request(SOURCE)
    except OSError:
        log=open(out/'source-run.log','ab')
        children.append(subprocess.Popen(['docker','exec',source,'python3','/bn006_bgp_speaker.py'],stdout=log,stderr=log))
    wait('all lab BGP sessions established',lambda: all(v['established'] for v in request(SOURCE).values()),180)

def pair_ready(med=None):
    a=routes('10.77.1.0/24','0:0');b=routes('10.77.2.0/24','65001:100')
    if {r['view'] for r in a}!=VIEWS or {r['view'] for r in b}!=VIEWS or len(a)!=2 or len(b)!=2:return False
    if med is not None and not all(r['med']==med for r in a):return False
    return {'global':a,'blue':b}

try:
    print('1. Verify actual received prefixes, views and RD isolation',flush=True)
    start_source();request(SOURCE,{'session':'all','present':True,'med':0})
    initial=wait('initial post-policy/Loc-RIB routes',lambda:pair_ready(0))
    assert all(r['local_pref']==100 and r['as_path']=='65002' for r in initial['global'])
    assert not routes('10.77.1.0/24','65001:100') and not routes('10.77.2.0/24','0:0')
    report['phases']['initial']=save('gate-initial',initial)
    print('2. Independent wire MED update triggers startup-configured import policy',flush=True)
    request(SOURCE,{'session':'default','present':True,'med':150})
    modified=wait('MED150 and LP250 policy branch',lambda:(lambda x:x if x and all(r['local_pref']==250 for r in x['global']) else False)(pair_ready(150)))
    assert all(r['med']==0 and r['local_pref']==100 for r in modified['blue'])
    raw=cmd(['docker','exec',eos,'Cli','-p','15','-c','show bgp ipv4 unicast 10.77.1.0/24 | json'])
    (out/'eos-modified-route.json').write_text(raw)
    cli=json.loads(raw)
    paths=cli['vrfs']['default']['bgpRouteEntries']['10.77.1.0/24']['bgpRoutePaths']
    assert len(paths)==1
    assert paths[0]['localPreference']==250 and paths[0]['med']==150
    assert paths[0]['routeType']['active'] and paths[0]['asPathEntry']['asPath']=='65002'
    report['phases']['modified']=save('gate-modified',modified)
    print('3. Actual withdrawal and reannouncement; BLUE stays present',flush=True)
    request(SOURCE,{'session':'default','present':False})
    wait('withdraw removes both global views',lambda:not routes('10.77.1.0/24','0:0'))
    assert len(routes('10.77.2.0/24','65001:100'))==2
    save('gate-withdrawn',api('routes',speaker=SPEAKER))
    request(SOURCE,{'session':'default','present':True,'med':150})
    wait('reannouncement',lambda:pair_ready(150))
    cursor=api('health')['durable_sequence']
    print('4. Ungraceful exporter loss: real TCP EOF, retained RIB and explicit gap',flush=True)
    cmd(['docker','kill','--signal','KILL',eos],log='exporter-kill.log')
    gap=wait('EOF gap',lambda:(lambda h:h if h['completeness_state']=='gap' else False)(api('health')))
    retained=pair_ready(150);assert retained
    assert all(r['completeness_state']=='gap' for r in retained['global']+retained['blue'])
    events=api('events',after_seq=cursor,speaker=SPEAKER,limit=1000)
    assert 'withdraw' not in json.dumps(events.get('events',[])).lower()
    report['phases']['gap']=save('gate-gap',{'health':gap,'routes':retained,'events':events})
    print('5. Recreate only the owned lab; exporter reconnects on a later epoch',flush=True)
    cmd(['containerlab','destroy','-t',str(root/'topology.clab.yml')],log='gate-destroy.log')
    cmd(['containerlab','deploy','-t',str(root/'topology.clab.yml')],log='gate-redeploy.log')
    start_source()
    restored=wait('later epoch and restored routes',lambda:(lambda x:x if x and all(r['source']['session_epoch']>=2 for r in x['global']+x['blue']) else False)(pair_ready(0)),180)
    assert len(restored['global'])==2 and len(restored['blue'])==2
    report['phases']['reconnect']=save('gate-reconnect',{'routes':restored,'health':api('health')})
    history=api('prefix-history',speaker=SPEAKER,prefix='10.77.1.0/24',vrf_rd='0:0',limit=1000)
    save('gate-prefix-history',history)
    raw=[]
    for line in (journal/'bmp.jsonl').read_text().splitlines():
        item=json.loads(line)
        if 'raw_frame_hex' in item:raw.append(bytes.fromhex(item['raw_frame_hex']))
    stream=b''.join(raw);(out/'eos-live-bmp.bin').write_bytes(stream)
    report.update(passed=True,raw_frame_count=len(raw),raw_stream_sha256=hashlib.sha256(stream).hexdigest(),
                  raw_stream_bytes=len(stream),limitations=['Observed EOS profile exports post-policy and Loc-RIB, not a pre-policy claim.','No FIB/data-plane reachability claim.','Reconnect is a real full dedicated-lab restart after abrupt exporter death.'])
    print('PASS: EOS BMP/RIB lifecycle, live RD isolation, policy update, EOF gap and reconnect',flush=True)
finally:
    (out/'live-gate-results.json').write_text(json.dumps(report,indent=2)+'\n')
