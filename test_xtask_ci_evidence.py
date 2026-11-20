# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

import copy
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('ci_evidence',ROOT/'xtask-ci-evidence.py')
ci=importlib.util.module_from_spec(spec);spec.loader.exec_module(ci)
COMMIT='a'*40


def completed(**fields):
    return dict(status='completed',conclusion='success',**fields)


def run():
    return completed(id=22,run_attempt=1,head_sha=COMMIT,workflow_id=7,
        path='.github/workflows/ci.yml',head_branch='main',event='push',
        repository={'full_name':ci.REPO},head_repository={'full_name':ci.REPO})


def jobs():
    names=['Select checks','hygiene','CI gate','musl (libc portability)']
    rows=[completed(name=n,id=i,run_attempt=1,steps=[]) for i,n in enumerate(names)]
    rows[2]['steps']=[completed(name='Require every selected check to succeed')]
    rows[3]['steps']=[completed(name='Check against musl')]
    for host,(runner,_) in ci.HOSTS.items():
        steps=ci.RUST_STEPS|({'Format','Executable recipe authoring manual'} if host=='Linux' else set())
        rows.append(completed(name=f'test ({runner})',id=len(rows),run_attempt=1,
                              steps=[completed(name=n) for n in steps]))
    return rows


class FakeClient:
    def __init__(self):
        self.runs=[run()];self.current=run();self.jobs=jobs();self.reads=0

    def get(self,path):
        if path=='actions/workflows/ci.yml': return {'id':7}
        if path=='actions/runs/22':
            self.reads+=1
            return copy.deepcopy(self.current)
        raise AssertionError(path)

    def pages(self,path,key):
        return copy.deepcopy(self.runs if key=='workflow_runs' else self.jobs)


class EvidenceTests(unittest.TestCase):
    def test_job_tokens_are_opaque_not_limited_to_personal_token_shape(self):
        for token in ['ghs_'+'a'*40, 'github_pat_'+'b'*90,
                      'header.'+'encoded-segment_'*40+'.signature~+/==']:
            with self.subTest(length=len(token)), patch.dict(os.environ, {'GH_TOKEN':token}):
                self.assertEqual(ci.Client().token, token)

    def test_missing_oversized_or_header_unsafe_tokens_are_refused_without_echo(self):
        for token in ['', 'a'*16385, 'secret\r\nInjected: value', 'secret token',
                      'secret\tvalue', 'secret\x7fvalue', 'secret\u00e9value']:
            with self.subTest(length=len(token)), patch.dict(os.environ, {'GH_TOKEN':token}):
                with self.assertRaises(ValueError) as caught:
                    ci.Client()
                self.assertNotIn(token or 'Injected', str(caught.exception))
        with patch.dict(os.environ, {}, clear=True), self.assertRaises(ValueError):
            ci.Client()

    def test_exact_full_main_run_is_accepted(self):
        proof=ci.select_run(FakeClient(),COMMIT)
        self.assertEqual(proof['run_id'],22)
        self.assertEqual(len(proof['jobs']),7)

    def test_wrong_commit_workflow_branch_event_or_repository_is_refused(self):
        changes=[{'head_sha':'b'*40},{'workflow_id':8},{'path':'.github/workflows/other.yml'},
                 {'head_branch':'feature'},{'event':'pull_request'},{'event':'pull_request_target'},
                 {'head_repository':{'full_name':'attacker/kosha'}},{'repository':{'full_name':'other/kosha'}}]
        for change in changes:
            with self.subTest(change=change):
                client=FakeClient();client.runs[0].update(change)
                with self.assertRaises(ValueError): ci.select_run(client,COMMIT)

    def test_latest_failure_or_pending_is_not_hidden_by_older_success(self):
        for status,conclusion in [('completed','failure'),('in_progress',None),('completed','cancelled')]:
            client=FakeClient();client.runs.insert(0,dict(run(),id=21))
            client.current.update(status=status,conclusion=conclusion)
            with self.assertRaises(ValueError): ci.select_run(client,COMMIT)

    def test_docs_only_missing_duplicate_skipped_and_mixed_attempt_jobs_fail(self):
        for mutate in [lambda r:r.pop(),lambda r:r.append(copy.deepcopy(r[-1])),
                       lambda r:r[-1].update(conclusion='skipped'),lambda r:r[-1].update(run_attempt=2),
                       lambda r:r[-1]['steps'][0].update(conclusion='skipped')]:
            client=FakeClient();mutate(client.jobs)
            with self.assertRaises(ValueError): ci.select_run(client,COMMIT)

    def test_rerun_race_after_jobs_fails(self):
        client=FakeClient();original=client.get
        def changing(path):
            value=original(path)
            if client.reads==2 and path=='actions/runs/22': value['run_attempt']=2
            return value
        client.get=changing
        with self.assertRaises(ValueError): ci.select_run(client,COMMIT)

    def reports(self,root,folder,proof):
        for host,(_,target) in ci.HOSTS.items():
            row={k:proof[k] for k in ('schema','repository','commit','run_id','run_attempt')}
            row.update(host=host,contracts=ci.hashes(root),rustflags='-D warnings',encoded_rustflags='',
                       rustc=f'rustc 1.100.0\nhost: {target}\nLLVM version: 21',image_os=host,image_version='123')
            path=folder/f'ci-validation-{host}-1'/'validation.json'
            path.parent.mkdir(parents=True);path.write_text(json.dumps(row))

    def test_reports_bind_exact_attempt_host_flags_and_workflow_contract(self):
        with tempfile.TemporaryDirectory() as temp:
            folder=Path(temp).resolve()
            proof=ci.select_run(FakeClient(),COMMIT)
            self.reports(ROOT,folder,proof)
            merged=ci.collect_reports(ROOT,folder,proof)
            self.assertEqual(set(merged['hosts']),set(ci.HOSTS))
            path=folder/'ci-validation-Linux-1/validation.json'
            original=json.loads(path.read_text())
            for change in [{'run_attempt':2},{'commit':'b'*40},{'host':'Windows'},
                           {'rustflags':''},{'encoded_rustflags':'unreviewed'}, {'contracts':{}},
                           {'rustc':'host: unexpected-target'}]:
                path.write_text(json.dumps(dict(original,**change)))
                with self.subTest(change=change),self.assertRaises(ValueError): ci.collect_reports(ROOT,folder,proof)

    def test_compatible_host_reuses_checks_and_drift_falls_back(self):
        evidence={'hosts':{'Windows':{'rustc':'rustc fixture','image_os':'win','image_version':'1'}}}
        self.assertTrue(ci.reuse_for_host(evidence,'Windows','rustc fixture','win','1'))
        for rustc,image,version in [('new rustc','win','1'),('rustc fixture','win','2'),('rustc fixture','','')]:
            self.assertFalse(ci.reuse_for_host(evidence,'Windows',rustc,image,version))

    def test_local_or_wrong_workflow_cannot_enable_reuse(self):
        with patch.dict(os.environ,{},clear=True):
            with self.assertRaises(ValueError): ci.actions_context(ROOT,release=True)
        env={'GITHUB_ACTIONS':'true','GITHUB_REPOSITORY':ci.REPO,'GITHUB_SHA':COMMIT,
             'GITHUB_REF':'refs/tags/v0.10.8','GITHUB_REF_TYPE':'tag','GITHUB_EVENT_NAME':'push',
             'GITHUB_WORKFLOW_REF':ci.REPO+'/.github/workflows/ci.yml@refs/tags/v0.10.8'}
        with patch.dict(os.environ,env,clear=True),patch.object(ci.subprocess,'check_output',side_effect=lambda argv,**kw: COMMIT if 'HEAD' in argv else ''):
            with self.assertRaises(ValueError): ci.actions_context(ROOT,release=True)

    def test_missing_reports_or_oversized_report_fail_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            folder=Path(temp).resolve();proof=ci.select_run(FakeClient(),COMMIT)
            with self.assertRaises(OSError): ci.collect_reports(ROOT,folder,proof)
            self.reports(ROOT,folder,proof)
            (folder/'ci-validation-Linux-1/validation.json').write_text(' '*65537)
            with self.assertRaises(ValueError): ci.collect_reports(ROOT,folder,proof)

    def test_summary_is_revalidated_live_and_cannot_be_edited_independently(self):
        with tempfile.TemporaryDirectory() as temp:
            folder=Path(temp).resolve();proof=ci.select_run(FakeClient(),COMMIT)
            reports=folder/'reports';self.reports(ROOT,reports,proof)
            evidence=ci.collect_reports(ROOT,reports,proof)
            path=folder/'ci-evidence.json';path.write_text(json.dumps(evidence))
            with patch.object(ci,'actions_context',return_value=COMMIT):
                self.assertEqual(ci.verified(ROOT,path,FakeClient()),evidence)
                edited=copy.deepcopy(evidence);edited['hosts']['Windows']['rustc']='forged'
                path.write_text(json.dumps(edited))
                with self.assertRaises(ValueError): ci.verified(ROOT,path,FakeClient())
                path.write_text(json.dumps(evidence))
                client=FakeClient();client.current['conclusion']='failure'
                with self.assertRaises(ValueError): ci.verified(ROOT,path,client)

    def test_release_context_accepts_only_exact_public_tag_checkout(self):
        env={'GITHUB_ACTIONS':'true','GITHUB_REPOSITORY':ci.REPO,'GITHUB_SHA':COMMIT,
             'GITHUB_REF':'refs/tags/v0.10.8','GITHUB_REF_TYPE':'tag','GITHUB_EVENT_NAME':'push',
             'GITHUB_WORKFLOW_REF':ci.REPO+'/.github/workflows/release.yml@refs/tags/v0.10.8'}
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp).resolve()
            with patch.dict(os.environ,env,clear=True),patch.object(ci.subprocess,'check_output',side_effect=lambda argv,**kw: COMMIT if 'HEAD' in argv else ''):
                self.assertEqual(ci.actions_context(root,release=True),COMMIT)
                (root/'publication').mkdir();(root/'publication/files.jsonl').write_text('private')
                with self.assertRaises(ValueError): ci.actions_context(root,release=True)

    def test_manifest_provenance_is_bound_to_approved_ci(self):
        with tempfile.TemporaryDirectory() as temp:
            folder=Path(temp).resolve();proof=ci.select_run(FakeClient(),COMMIT)
            row={k:proof[k] for k in ('repository','commit','run_id','run_attempt','url')}
            row['reused_rust_validation']=True
            for _,target in ci.HOSTS.values():
                (folder/f'release-manifest-{target}.json').write_text(json.dumps({'ci_validation':row}))
            ci.check_manifests(folder,proof)
            path=folder/'release-manifest-x86_64-pc-windows-msvc.json'
            for changes in [{'run_id':21},{'commit':'b'*40},{'reused_rust_validation':'true'}]:
                path.write_text(json.dumps({'ci_validation':dict(row,**changes)}))
                with self.assertRaises(ValueError): ci.check_manifests(folder,proof)

    def test_workflow_wiring_preserves_reports_and_keeps_them_out_of_release_assets(self):
        workflow=(ROOT/'.github/workflows/ci.yml').read_text()
        for name in ci.RUST_STEPS:
            self.assertIn('name: '+name,workflow)
        self.assertEqual(workflow.count("if: github.repository == 'coolnew/kosha' && github.ref == 'refs/heads/main'"),2)
        self.assertIn('path: ${{ runner.temp }}/ci-validation/validation.json',workflow)
        self.assertNotIn('--native target/',workflow)
        release=(ROOT/'.github/workflows/release.yml').read_text()
        self.assertIn('pattern: kosha-*',release)
        self.assertIn('name: validated-ci',release)
        self.assertIn('path: target/validated-ci/\n          overwrite: true',release)
        self.assertIn('path: target/release-artifacts',release)
        self.assertNotIn('path: artifacts',release)
        commands=[line for line in release.splitlines() if 'run: python xtask-release.py --execute' in line]
        self.assertEqual(len(commands),3)
        self.assertTrue(all("github.event_name == 'push' && github.ref_type == 'tag'" in line and '--ci-evidence' in line for line in commands))

    def test_api_pagination_does_not_silently_truncate(self):
        client=object.__new__(ci.Client)
        client.get=lambda _: {'jobs':[],'total_count':1}
        with self.assertRaises(ValueError): client.pages('actions/runs/22/jobs?filter=latest','jobs')
        client.get=lambda _: {'jobs':[{}]*100,'total_count':1001}
        with self.assertRaises(ValueError): client.pages('actions/runs/22/jobs?filter=latest','jobs')

    def test_api_rejects_cross_repo_urls_and_redirects(self):
        with patch.dict(os.environ,{'GH_TOKEN':'fake_test_token_for_ci_only'}): client=ci.Client()
        for path in ['https://evil.invalid/','actions/../secrets','actions/jobs?token=secret%20']:
            with self.assertRaises(ValueError): client.get(path)
        with self.assertRaises(ValueError): ci.NoRedirect().redirect_request(None,None,None,None,None,None)

    def test_invalid_header_token_is_rejected_without_exposure(self):
        token='private_token_value\nheader_injection'
        with patch.dict(os.environ,{'GH_TOKEN':token}),self.assertRaises(ValueError) as error:
            ci.Client()
        self.assertNotIn('private_token_value',str(error.exception))
