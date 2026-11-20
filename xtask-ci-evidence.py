#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Verify exact-commit trusted main CI and its native reports. Read-only GitHub API; no polling."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener

ROOT = Path(__file__).resolve().parent
REPO = 'coolnew/kosha'
SCHEMA = 'kosha.ci-validation.v1'
HOSTS = {'Linux':('ubuntu-latest','x86_64-unknown-linux-gnu'),
         'Windows':('windows-latest','x86_64-pc-windows-msvc'),
         'macOS':('macos-latest','aarch64-apple-darwin')}
RUST_STEPS = {'Check core crates','Test core crates','Build everything','Test GUI','Record reusable native validation','Upload native validation report'}
SHA = re.compile(r'[0-9a-f]{40}')


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ValueError('GitHub API redirect refused')


class Client:
    def __init__(self):
        self.token = os.environ.get('GH_TOKEN')
        # Job tokens are opaque and may be longer or use a different encoding
        # than personal access tokens. Check header safety, not a guessed format;
        # GitHub authenticates the token and enforces Actions read permission.
        if (not self.token or len(self.token) > 16384 or not self.token.isascii()
                or any(not c.isprintable() or c.isspace() for c in self.token)):
            raise ValueError('A nonempty header-safe GH_TOKEN is required; no private .env is read')
        self.opener = build_opener(NoRedirect())

    def get(self, path):
        if not re.fullmatch(r'actions/[A-Za-z0-9_./?=&-]+',path) or '..' in path:
            raise ValueError('Unexpected Actions API path')
        request = Request('https://api.github.com/repos/'+REPO+'/'+path,headers={
            'Authorization':'Bearer '+self.token,'Accept':'application/vnd.github+json',
            'X-GitHub-Api-Version':'2022-11-28','User-Agent':'kosha-ci-evidence'})
        try:
            with self.opener.open(request,timeout=30) as response:
                raw = response.read(2*1024*1024+1)
                if len(raw)>2*1024*1024: raise ValueError('Actions API response too large')
                return json.loads(raw)
        except HTTPError as error:
            code = error.code
            error.close()
            raise ValueError(f'Actions API HTTP {code}; no automatic retry or validation bypass') from None
        except URLError:
            raise ValueError('Actions API unavailable; no automatic retry or validation bypass') from None

    def pages(self,path,key):
        values=[]
        for page in range(1,11):
            data=self.get(path+f'&per_page=100&page={page}')
            part=data[key]
            if not isinstance(part,list): raise ValueError('Unexpected Actions pagination')
            values.extend(part)
            if len(part)<100:
                if data.get('total_count',len(values)) != len(values): raise ValueError('Incomplete Actions inventory')
                return values
        raise ValueError('Actions pagination limit exceeded')


def success(row):
    return row.get('status')=='completed' and row.get('conclusion')=='success'


def run_identity(run,sha,workflow_id):
    return (run.get('head_sha')==sha and run.get('workflow_id')==workflow_id
            and run.get('path')=='.github/workflows/ci.yml'
            and run.get('head_branch')=='main' and run.get('event') in {'push','workflow_dispatch'}
            and run.get('repository',{}).get('full_name')==REPO
            and run.get('head_repository',{}).get('full_name')==REPO)


def check_jobs(jobs,attempt):
    expected={'Select checks','hygiene','CI gate','musl (libc portability)'} | {f'test ({v[0]})' for v in HOSTS.values()}
    selected={}
    for name in expected:
        found=[j for j in jobs if j.get('name')==name]
        if len(found)!=1 or not success(found[0]) or found[0].get('run_attempt')!=attempt:
            raise ValueError('Full successful current-attempt CI required: '+name)
        selected[name]=found[0]
    for host,(runner,_) in HOSTS.items():
        needed=RUST_STEPS | ({'Format','Executable recipe authoring manual'} if host=='Linux' else set())
        steps=selected[f'test ({runner})'].get('steps',[])
        for name in needed:
            found=[s for s in steps if s.get('name')==name]
            if len(found)!=1 or not success(found[0]): raise ValueError('CI step missing/skipped/failed: '+name)
    for job,name in [('musl (libc portability)','Check against musl'),
                     ('CI gate','Require every selected check to succeed')]:
        found=[s for s in selected[job].get('steps',[]) if s.get('name')==name]
        if len(found)!=1 or not success(found[0]): raise ValueError('CI step missing/skipped/failed: '+name)
    return {name:row['id'] for name,row in selected.items()}


def select_run(client,sha):
    if not SHA.fullmatch(sha): raise ValueError('Expected full commit SHA')
    workflow=client.get('actions/workflows/ci.yml')
    runs=client.pages(f'actions/workflows/{workflow["id"]}/runs?head_sha={sha}', 'workflow_runs')
    trusted=[r for r in runs if run_identity(r,sha,workflow['id'])]
    if not trusted: raise ValueError('No trusted main CI for this exact commit; run full CI on main first')
    latest=max(trusted,key=lambda r:r['id'])
    run=client.get(f'actions/runs/{latest["id"]}')
    if run.get('id')!=latest['id'] or not run_identity(run,sha,workflow['id']) or not success(run):
        raise ValueError('Latest trusted CI is pending/failed/cancelled; complete or rerun CI before release')
    jobs=client.pages(f'actions/runs/{run["id"]}/jobs?filter=latest','jobs')
    ids=check_jobs(jobs,run['run_attempt'])
    again=client.get(f'actions/runs/{run["id"]}')
    if (not success(again) or not run_identity(again,sha,workflow['id'])
            or again.get('id')!=run['id'] or again.get('run_attempt')!=run['run_attempt']):
        raise ValueError('CI changed while checking; stop and review')
    return {'schema':SCHEMA,'repository':REPO,'commit':sha,'run_id':run['id'],
            'run_attempt':run['run_attempt'],'workflow_id':workflow['id'],
            'url':f'https://github.com/{REPO}/actions/runs/{run["id"]}', 'jobs':ids}


def actions_context(root,release=False):
    env=os.environ
    if env.get('GITHUB_ACTIONS')!='true' or env.get('GITHUB_REPOSITORY')!=REPO:
        raise ValueError('CI reuse is restricted to the public repository Actions workflow')
    sha=env.get('GITHUB_SHA','')
    if not SHA.fullmatch(sha) or subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()!=sha:
        raise ValueError('Actions SHA does not match checkout')
    if subprocess.check_output(['git','status','--porcelain'],cwd=root,text=True).strip():
        raise ValueError('Actions source changed; CI evidence requires a clean checkout')
    if release:
        expected=f'{REPO}/.github/workflows/release.yml@'+env.get('GITHUB_REF','')
        if (env.get('GITHUB_EVENT_NAME')!='push' or env.get('GITHUB_REF_TYPE')!='tag'
                or not env.get('GITHUB_REF','').startswith('refs/tags/v')
                or env.get('GITHUB_WORKFLOW_REF')!=expected
                or (root/'publication/files.jsonl').exists()):
            raise ValueError('Only the reviewed public tag release workflow may reuse CI validation')
    return sha


def hashes(root):
    return {n:hashlib.sha256((root/n).read_bytes()).hexdigest()
            for n in ('.github/workflows/ci.yml','xtask-ci-evidence.py')}


def native_report(root):
    sha=actions_context(root)
    host=os.environ.get('RUNNER_OS')
    if host not in HOSTS: raise ValueError('Unexpected native CI host')
    return {'schema':SCHEMA,'repository':REPO,'commit':sha,'host':host,
            'run_id':int(os.environ['GITHUB_RUN_ID']),'run_attempt':int(os.environ['GITHUB_RUN_ATTEMPT']),
            'rustc':subprocess.check_output(['rustc','-vV'],text=True).strip(),
            'image_os':os.environ.get('ImageOS',''),'image_version':os.environ.get('ImageVersion',''),
            'rustflags':os.environ.get('RUSTFLAGS',''),'encoded_rustflags':os.environ.get('CARGO_ENCODED_RUSTFLAGS',''),
            'contracts':hashes(root)}


def collect_reports(root,folder,evidence):
    reports={}
    for host,(_,target) in HOSTS.items():
        path=folder/f'ci-validation-{host}-{evidence["run_attempt"]}'/'validation.json'
        if path.stat().st_size>65536: raise ValueError('Native CI report exceeds limit')
        row=json.loads(path.read_text(encoding='utf-8'))
        for key in ('schema','repository','commit','run_id','run_attempt'):
            if row.get(key)!=evidence[key]: raise ValueError('CI report provenance mismatch: '+key)
        if (row.get('host')!=host or row.get('contracts')!=hashes(root)
                or row.get('rustflags')!='-D warnings' or row.get('encoded_rustflags')!=''
                or '\nhost: '+target+'\n' not in '\n'+row.get('rustc','')+'\n'):
            raise ValueError('CI report host/contract/flags mismatch')
        reports[host]=row
    return dict(evidence,hosts=reports)


def verified(root,path,client=None):
    sha=actions_context(root,release=True)
    if path.stat().st_size>256*1024: raise ValueError('CI evidence exceeds limit')
    evidence=json.loads(path.read_text(encoding='utf-8'))
    current=select_run(client or Client(),sha)
    if any(evidence.get(k)!=v for k,v in current.items()): raise ValueError('CI evidence no longer matches live approved run')
    # The original reports are also retained beside the summary; do not trust a
    # hand-edited summary or a file obtained from an unrelated workflow run.
    expected=collect_reports(root,path.parent/'reports',current)
    if evidence!=expected: raise ValueError('CI summary differs from native reports')
    return evidence


def reuse_for_host(evidence,host,rustc,image_os,image_version):
    row=evidence['hosts'].get(host)
    if row is None: raise ValueError('No CI report for this release host')
    return bool(image_os and image_version and row['rustc']==rustc
                and row['image_os']==image_os and row['image_version']==image_version)


def check_manifests(folder,evidence):
    for _,target in HOSTS.values():
        path=folder/f'release-manifest-{target}.json'
        if path.stat().st_size>1024*1024: raise ValueError('Host manifest exceeds limit')
        row=json.loads(path.read_text(encoding='utf-8')).get('ci_validation',{})
        if (any(row.get(k)!=evidence[k] for k in ('repository','commit','run_id','run_attempt','url'))
                or type(row.get('reused_rust_validation')) is not bool):
            raise ValueError('Host manifest does not bind the approved CI evidence: '+target)


def write(path,data):
    path.parent.mkdir(parents=True,exist_ok=True)
    with path.open('x',encoding='utf-8',newline='\n') as stream:
        json.dump(data,stream,indent=2);stream.write('\n')


def main():
    p=argparse.ArgumentParser(description=__doc__)
    group=p.add_mutually_exclusive_group(required=True)
    group.add_argument('--native',type=Path,help='CI: write native validation report after successful tests')
    group.add_argument('--select',type=Path,help='Tag gate: select approved CI and write its identity')
    group.add_argument('--collect',type=Path,help='Tag gate: identity file to combine with downloaded reports')
    group.add_argument('--verify',type=Path,help='Recheck evidence against live CI; no waiting or fallback on failure')
    p.add_argument('--reports',type=Path)
    p.add_argument('--output',type=Path)
    p.add_argument('--manifests',type=Path,help='With --verify: check host manifests bind this CI approval')
    a=p.parse_args()
    try:
        if a.manifests and not a.verify: raise ValueError('--manifests requires --verify')
        if a.native: write(a.native,native_report(ROOT))
        elif a.select:
            row=select_run(Client(),actions_context(ROOT,release=True));write(a.select,row)
            with open(os.environ['GITHUB_OUTPUT'],'a',encoding='utf-8') as out:
                out.write(f'run_id={row["run_id"]}\nrun_attempt={row["run_attempt"]}\n')
        elif a.collect:
            if not a.reports or not a.output: raise ValueError('Collect requires --reports and --output')
            row=json.loads(a.collect.read_text())
            live=select_run(Client(),actions_context(ROOT,release=True))
            if row!=live: raise ValueError('Selected CI changed before report collection')
            write(a.output,collect_reports(ROOT,a.reports,live))
        else:
            row=verified(ROOT,a.verify)
            if a.manifests: check_manifests(a.manifests,row)
            print(f'Approved CI: {row["url"]}, attempt {row["run_attempt"]}, commit {row["commit"]}')
        return 0
    except (ValueError,OSError,KeyError,TypeError,subprocess.SubprocessError) as error:
        print('CI evidence refused: '+str(error),file=sys.stderr)
        return 1


if __name__=='__main__':
    raise SystemExit(main())
