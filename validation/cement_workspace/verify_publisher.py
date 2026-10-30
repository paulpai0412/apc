"""Actual bounded research-to-runtime publication, not fabricated candidate lineage.

A second full history-driven study supplies a qualified immutable template. A
separately generated native quarter (new frozen seed) triggers the real research
scheduler. Only its genuinely auto-sealed, qualified candidate may be published.
Failure to improve/qualify is evidence of retained baseline, NEVER a test pass.
"""
import copy
from datetime import datetime, timedelta, timezone
import json
import time
import uuid

from workspace import SAMPLES, generate, require, save, sha
from verify_api import identifier, wait_run
from verify_runtime import approve_install, await_operation, current, healthy, operation, stamp


def verify(research, runtime, reviewer, root, output, profile, original, deadline):
    directory = output / 'publisher'
    directory.mkdir()
    evidence = {'status': 'running', 'factory_qualification': False, 'samples_per_research_run': SAMPLES}
    policy = None
    source_policy = None
    try:
        dataset_id = original['study']['dataset_id']
        draft = research.call('/api/research/draft-study', {'dataset_id': dataset_id})
        config = draft['config']
        config.update(id=profile['engine_id'], engineering_confirmed=True,
                      variables=copy.deepcopy(original['study']['config']['variables']),
                      controller=copy.deepcopy(original['study']['config']['controller']))
        require(config['plant_source'] == 'identified_history' and config['sample_time'] == 60
                and config['data_window']['samples'] == SAMPLES, 'Publisher source must retain actual full-quarter history and original Ts')
        settings = copy.deepcopy(original['study']['research'])
        settings['selection_mode'] = 'manual'
        study = research.call('/api/research/studies', {'name': 'Cement observational frozen-model publication qualification',
            'dataset_id': dataset_id, 'config': config, 'research': settings})
        model = research.call('/api/research/identify', {'study_id': study['id']})
        require(model['qualification']['historical_prediction'] is True and model['qualification']['factory_authority'] is False,
                'Historical model lacks honest predictive-only qualification')
        study = research.call('/api/research/studies', {'id': study['id'], 'name': study['name'], 'dataset_id': dataset_id,
            'model_id': model['id'], 'config': config, 'research': settings})
        run = research.call('/api/research/runs', {'study_id': study['id']})
        evidence['template_run_id'] = run['id']
        save(directory / 'template-run-identity.json', {'run_id': run['id'], 'study_id': study['id'], 'model_id': model['id']})
        record = wait_run(research, run['id'], False, evidence, deadline)
        from verify_autoresearch import check_run
        evidence['template_research'] = check_run(root / 'research-store', run['id'])
        require(evidence['template_research']['samples_per_method'] == SAMPLES, 'Template qualification omitted native quarter')
        # Method order was frozen before sealed qualification; never rank methods
        # by their holdout score to manufacture a publication opportunity.
        method = settings['methods'][0]
        chosen = next(row for row in record['results']['methods'] if row['method'] == method)
        require(chosen['recommendation'] == 'candidate_for_review',
                'Predeclared publication template did not qualify; no alternate holdout winner is substituted')
        template = approve_install(research, runtime, reviewer, profile, record['id'], method, deadline, directory)
        evidence['template'] = template
        operation(runtime, profile['profile_hash'], {'kind': 'switch_version', 'candidate_hash': template['version']['id'],
                  'approval_id': template['approval']['approval_id']}, deadline, directory)
        before = healthy(runtime, profile['profile_hash'], deadline)
        require(before['version'] == template['version']['model_sha256'], 'Publication baseline is not the exact installed native model')
        # Policy initial tuning reproduces the immutable template exactly. Physics,
        # node IDs, safety bounds and Ts are retained from its frozen model.
        settings['initial'] = copy.deepcopy(chosen['parameters'])
        policy_study = research.call('/api/research/studies', {'name': 'Cement bounded template-preserving new-data policy',
            'dataset_id': dataset_id, 'model_id': model['id'], 'config': config, 'research': settings})
        source_policy = research.call('/api/research/policies', {
            'name': 'Disposable cement one-new-quarter frozen-model tuning', 'study_id': policy_study['id'],
            'model_id': model['id'], 'action': 'frozen_model_tuning', 'trigger': {'kind': 'new_data'}, 'enabled': True,
            'minimum_rows': SAMPLES, 'max_filled_fraction': 0.01, 'cooldown_seconds': 30, 'max_runs': 1,
            'max_trials_total': len(settings['methods']) * (1 + 2 * settings['rounds']),
            'publication_template_version_id': template['version']['id']})
        evidence['source_policy'] = source_policy
        snapshot = runtime.call('/api/runtime/publisher')
        source = next(row for row in snapshot['source_policies'] if row['id'] == source_policy['id'])
        # Numerical bounds are derived from the frozen research domain and the
        # template's actual selected scales, not broad arbitrary field permission.
        # Untuned slack/target weights and horizons remain numerically unchanged.
        allowlist = []
        for row in source['tuning_parameters']:
            pointer, value = row['pointer'], row['value']
            if pointer.endswith('/alpha'):
                low, high = settings['bounds']['alpha']
            elif pointer.startswith('/variables/cvs/') and pointer.endswith('/weight'):
                scale = settings['initial']['cv_weight_scale']
                low, high = [value * bound / scale for bound in settings['bounds']['cv_weight_scale']]
            elif pointer.startswith('/variables/mvs/') and pointer.endswith('/weight_r'):
                scale = settings['initial']['mv_weight_scale']
                low, high = [value * bound / scale for bound in settings['bounds']['mv_weight_scale']]
            else:
                low = high = value
            allowlist.append({'pointer': pointer, 'minimum': low, 'maximum': high, 'max_delta': max(high - low, 0.001)})
        spec = {'profile_hash': profile['profile_hash'], 'target': profile['target'],
            'baseline_candidate_hash': template['version']['id'], 'baseline_model_sha256': template['version']['model_sha256'],
            'source_policy_id': source['id'], 'source_policy_revision': source['revision'],
            'source_policy_sha256': source['definition_sha256'], 'source_study_id': source['study_id'],
            'allowlist': allowlist, 'operating_envelope': [
                {'tag': row['pv_tag'], 'minimum': row['hard_low'], 'maximum': row['hard_high']} for row in profile['variables']],
            'minimum_observation_ms': 1000, 'minimum_good_samples': 2, 'cooldown_ms': 1000,
            'max_changes': 1, 'expires_at': stamp(datetime.now(timezone.utc) + timedelta(days=7))}
        policy = runtime.call('/api/runtime/publisher/policies', {'policy_id': 'cement-acceptance-' + uuid.uuid4().hex,
            'expected_revision': 0, 'reason': 'Explicit isolated one-change numerical grant specification', 'spec': spec})
        require(policy['state'] == 'paused', 'New publication policy must not self-enable')
        state_path = '/api/runtime/publisher/policies/' + policy['policy_id'] + '/state'
        grant = {'expected_revision': policy['revision'], 'state': 'enabled', 'reason': 'Independent disposable reviewer grants this exact bounded revision'}
        evidence['self_grant_rejection'] = runtime.call(state_path, grant, expected_error=(409,))
        unchanged = runtime.call('/api/runtime/publisher/policies/' + policy['policy_id'])['policy']
        require(unchanged['revision'] == policy['revision'] and unchanged['state'] == 'paused'
                and unchanged['grantor_id'] is None, 'Rejected self-grant changed the publication policy')
        policy = reviewer.call(state_path, grant)
        evidence['granted_policy'] = policy
        require(policy['grantor_id'] != policy['spec_author_id'], 'Publication grant is not independent')
        after = current(runtime, profile['profile_hash'])
        require(after['version'] == before['version'] and after['authority'] == before['authority'],
                'Enabling policy must not itself switch/start/arm/engage')
        # A genuinely different native-generated source, not renaming or adding a
        # byte to old observations to bypass the scheduler's novelty gate.
        generated = directory / 'new-quarter-source'
        generated.mkdir()
        workspace = json.loads((root / 'workspace.json').read_text())
        baseline_campaign = json.loads((root / 'data/campaign.json').read_text())
        generate(generated, root / 'source', workspace['binary'], workspace['python'], seed=baseline_campaign['seed'] + 1)
        raw_path = generated / 'data/cement-2026-q1-raw.csv'
        require(sha(raw_path) != sha(root / 'data/cement-2026-q1-raw.csv'), 'New source must contain genuinely different observations')
        raw = research.call('/api/research/import', {'name': 'Independent new-seed native cement quarter for policy trigger',
                            'format': 'csv', 'content': raw_path.read_bytes().decode('utf-8')})
        require(raw['source_sha256'] == sha(raw_path), 'Policy import lost raw-byte provenance')
        clean = research.call('/api/research/clean', {'dataset_id': identifier(raw),
            'cleaning': json.loads((generated / 'data/cleaning.json').read_text())})
        evidence['new_source'] = {'raw': raw, 'clean': clean, 'provenance': json.loads((generated / 'data/provenance.json').read_text())}
        while True:
            listing = research.call('/api/research/policies')
            source_policy = next(row for row in listing['policies'] if row['id'] == source_policy['id'])
            claims = [row for row in source_policy['state']['audit'] if row.get('run_id')]
            if claims:
                run_id = claims[0]['run_id']
                break
            require(time.monotonic() < deadline, 'Actual new-data scheduler did not claim full source: ' + json.dumps(source_policy))
            time.sleep(5)
        evidence['scheduled_run_id'] = run_id
        scheduled = wait_run(research, run_id, False, evidence, deadline)
        require(scheduled['study']['research']['selection_mode'] == 'automatic', 'Policy worker did not freeze its declared automatic selection mode')
        evidence['scheduled_research'] = check_run(root / 'research-store', run_id)
        require(evidence['scheduled_research']['samples_per_method'] == SAMPLES, 'Policy research omitted full native quarter')
        selected = next(row for row in scheduled['results']['methods'] if row['method'] == method)
        require(selected['recommendation'] == 'candidate_for_review', 'Real policy retained baseline; publication acceptance remains unexercised, not passed')
        while True:
            snapshot = runtime.call('/api/runtime/publisher')
            jobs = [job for job in snapshot['jobs'] if job['policy_id'] == policy['policy_id']]
            if jobs:
                job = jobs[0]
                require(job['state'] not in ('rejected', 'unknown'), 'Publication did not confirm native application; original ID only: ' + json.dumps(job))
                if job['state'] == 'accepted':
                    break
            require(time.monotonic() < deadline, 'Actual sealing/observation/publication unavailable: ' + json.dumps(snapshot))
            time.sleep(5)
        receipt = await_operation(runtime, job['operation_id'], deadline)
        published = healthy(runtime, profile['profile_hash'], deadline)
        require(published['version'] == job['model_sha256'] and published['authority'] == 'engaged',
                'Publisher acceptance lacks actual exact-digest active native authority')
        evidence.update(job=job, operation=receipt, publication_snapshot=snapshot, published_native=published['native_status'])
        policy = runtime.call(state_path, {'expected_revision': policy['revision'], 'state': 'paused',
            'reason': 'End bounded acceptance; no further automatic publication'})
        # Rollback remains an individually approved native transition; never fake
        # lineage asserting the old baseline was a newly generated policy winner.
        operation(runtime, profile['profile_hash'], {'kind': 'switch_version', 'candidate_hash': template['version']['id'],
                  'approval_id': template['approval']['approval_id']}, deadline, directory)
        require(healthy(runtime, profile['profile_hash'], deadline)['version'] == template['version']['model_sha256'], 'Approved rollback did not restore template')
        evidence['status'] = 'passed_actual_source_policy_and_native_publication'
        return evidence
    except Exception as error:
        evidence.update(status='failed', error=str(error))
        raise
    finally:
        # Owned policies only. Failed cleanup is evidence, not concealed or used
        # to overwrite the original failure; no uncertain command is replayed.
        for label, callback in (
            ('runtime', lambda: runtime.call('/api/runtime/publisher/policies/' + policy['policy_id'] + '/state',
                {'expected_revision': policy['revision'], 'state': 'paused', 'reason': 'Acceptance owner stops new publication work'}) if policy and policy['state'] == 'enabled' else None),
            ('research', lambda: research.call('/api/research/policies/' + source_policy['id'] + '/state',
                {'expected_revision': source_policy['revision'], 'enabled': False}) if source_policy and source_policy['enabled'] else None)):
            try:
                result = callback()
                if result is not None:
                    evidence[label + '_paused'] = result
            except Exception as cleanup_error:
                evidence[label + '_pause_error'] = str(cleanup_error)
        save(directory / 'manifest.json', evidence)
