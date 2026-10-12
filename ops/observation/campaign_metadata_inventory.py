"""Read-only campaign inventory. Never opens outcome archives or changes evidence."""
import argparse,hashlib,json,subprocess,os,itertools
from datetime import datetime,timezone
from pathlib import Path

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--repo',type=Path,required=True)
p.add_argument('--revision',required=True)
p.add_argument('--capture-root',type=Path,required=True)
a=p.parse_args()
env=dict(os.environ,GIT_NO_LAZY_FETCH='1')
promisor=subprocess.run(['git','config','--get-regexp',r'^remote\..*\.promisor$'],cwd=a.repo,env=env,capture_output=True,timeout=30)
if promisor.returncode not in (0,1) or promisor.stdout:raise RuntimeError('Read-only audit refuses promisor or unreadable Git configuration')
def git(*args):return subprocess.check_output(['git','--no-replace-objects',*args],cwd=a.repo,env=env,timeout=30)
commit=git('rev-parse',a.revision+'^{commit}').decode().strip()
path='campaign/step4-campaign-ledger.ndjson';spec=commit+':'+path
size=int(git('cat-file','-s',spec))
if size>2*1024*1024:raise RuntimeError('Ledger exceeds audit read bound')
body=git('cat-file','blob',spec)
if len(body)!=size:raise RuntimeError('Ledger length changed')
events=[]
for number,line in enumerate(body.splitlines(),1):
 if not line:raise RuntimeError('Empty ledger line '+str(number))
 row=json.loads(line)
 if not isinstance(row,dict) or not isinstance(row.get('event'),str):raise RuntimeError('Malformed campaign event')
 events.append(row['event'])
root_exists=a.capture_root.is_dir();listing_error=None
try:entries=[item.name for item in itertools.islice(a.capture_root.iterdir(),10001)] if root_exists else []
except OSError as error:entries=[];listing_error=type(error).__name__
truncated=len(entries)>10000;entries=sorted(entries[:10000])
report={'checked_at':datetime.now(timezone.utc).isoformat(),'read_only':True,'ledger_commit':commit,'ledger_git_blob':git('rev-parse',spec).decode().strip(),'ledger_sha256':hashlib.sha256(body).hexdigest(),'ledger_bytes':size,'campaign_event_count':len(events),'event_counts':{event:events.count(event) for event in sorted(set(events))},'campaign_state':'COLLECTING_EMPTY' if not events else 'REQUIRES_FROZEN_CONTROLLER_REPLAY','capture_root_present':root_exists,'capture_root_entries':entries,'capture_listing_atomic':False,'expected_outcome_cohort':'unavailable' if not events else 'not_derived_by_metadata_inventory','outcome_access':'not_attempted','outcome_coverage':'unestablished','orphan_findings':'not_evaluated_without_verified_expected_cohort','artifact_authentication':'not_claimed_from_directory_listing','efficacy':'not_evaluated'}
report.update(schema='campaign-metadata-audit-v1',requested_revision=a.revision,capture_root=str(a.capture_root),capture_listing_error=listing_error,capture_listing_truncated=truncated,capture_root_entry_count_lower_bound=len(entries),promisor_remote_refused=True,lazy_fetch_disabled=True)
print(json.dumps(report,indent=2))

