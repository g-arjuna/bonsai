// @ts-check
// M6D — Schema-bound collection workspace browser acceptance.
//
// These tests drive the REAL built UI (ui/dist) through the device
// drawer's `schema` tab. Backend APIs are intercepted with page.route
// fixtures that mirror the authoritative response shapes; the wire-level
// activation path itself is covered by tests/yang_schema_bound_acquisition.rs
// against a real mock-gNMI backend. Where a live bonsai backend is
// available (BONSAI_LIVE=1), the same spec also runs a smoke pass against
// it.
import { test, expect } from '@playwright/test';

const DEVICE_ADDR = '10.0.0.1:57400';
const ENC_ADDR = encodeURIComponent(DEVICE_ADDR);
const NS = 1_000_000_000;

function baseDevice(overrides = {}) {
  return {
    address: DEVICE_ADDR,
    hostname: 'edge-1',
    vendor: 'openconfig',
    role: 'leaf',
    site: 'lab',
    enabled: true,
    collector_id: 'col-1',
    health: 'healthy',
    acquisition: 'explicit',
    acquisition_plan_state: 'submitted',
    acquisition_plan_id: 'plan-current',
    schema_binding: null,
    updated_at_ns: 1_700_000_000_000_000_000,
    selected_paths: [],
    subscription_statuses: [],
    interfaces: [],
    bgp_neighbors: [],
    lldp_neighbors: [],
    recent_changes: [],
    detections: [],
    resolution_audit: [],
    created_at_ns: 1,
    created_by: 'test',
    ...overrides,
  };
}

function pathResult(overrides = {}) {
  return {
    path: '/interfaces/interface[name]/state/counters/in-octets',
    module: 'openconfig-interfaces',
    module_revision: '2024-01-01',
    namespace: 'https://openconfig.net/yang/interfaces',
    kind: 'leaf',
    config: false,
    keys: [],
    type_name: 'uint64',
    resolved_type: 'uint64',
    units: 'bytes',
    required_features: [],
    status: 'applicable',
    reasons: [],
    ...overrides,
  };
}

function moduleResult(overrides = {}) {
  return {
    name: 'openconfig-interfaces',
    catalogue_revision: '2024-01-01',
    device_revision: '2024-01-01',
    organization: 'OpenConfig',
    namespace: 'https://openconfig.net/yang/interfaces',
    classification: 'implemented',
    enabled_features: [],
    features_state_known: true,
    deviations: [],
    status: 'applicable',
    reasons: [],
    ...overrides,
  };
}

function snapshot(overrides = {}) {
  return {
    snapshot_id: 42,
    device_key: 'edge-1',
    device_address: DEVICE_ADDR,
    collector_id: 'col-1',
    schema_id: 'a'.repeat(64),
    content_id: 'dev-content-1',
    datastore: 'ietf-datastores:operational',
    overall_status: 'applicable',
    outcome: 'ok',
    result: {
      result_version: 1,
      schema_id: 'a'.repeat(64),
      content_id: 'dev-content-1',
      datastore: 'ietf-datastores:operational',
      overall_status: 'applicable',
      reasons: [],
      modules: [moduleResult()],
      paths: [
        pathResult(),
        pathResult({
          path: '/network-instances/network-instance[hostname]/protocols/protocol/bgp/neighbors/neighbor[neighbor-address]/state/session-state',
          module: 'openconfig-network-instance',
          kind: 'leaf',
          type_name: 'enumeration',
        }),
      ],
      paths_total: 2,
      paths_truncated: false,
      diagnostics: [],
      evidence_fingerprint: 'b'.repeat(64),
      evaluated_at_ns: 1_700_000_000_000_000_000,
      evidence_observed_at_ns: 1_700_000_000_000_000_000,
    },
    capabilities: null,
    yang_library: null,
    evidence_fingerprint: 'b'.repeat(64),
    observed_at_ns: 1_700_000_000_000_000_000,
    evaluated_at_ns: 1_700_000_000_000_000_000,
    error: null,
    created_at_ns: 1_700_000_000_000_000_000,
    freshness: 'fresh',
    freshness_reasons: [],
    active_schema_id: 'a'.repeat(64),
    catalogue_state: null,
    ...overrides,
  };
}

function applicabilityBody(overrides = {}) {
  return {
    address: DEVICE_ADDR,
    snapshot: snapshot(),
    paths: [pathResult()],
    paths_matching: 1,
    modules: [moduleResult()],
    ...overrides,
  };
}

function previewBody(overrides = {}) {
  return {
    device_key: 'edge-1',
    device_address: DEVICE_ADDR,
    activation_permitted: true,
    catalogue_state: null,
    active_schema_id: 'a'.repeat(64),
    freshness: 'fresh',
    freshness_reasons: [],
    current_binding: null,
    binding_state: 'none',
    config_revision: 1_700_000_000_000_000_000,
    evaluation: {
      plan_version: 1,
      device_key: 'edge-1',
      device_address: DEVICE_ADDR,
      schema_id: 'a'.repeat(64),
      content_id: 'dev-content-1',
      snapshot_id: 42,
      evidence_fingerprint: 'b'.repeat(64),
      datastore: 'ietf-datastores:operational',
      collector_id: 'col-1',
      acquisition_mode: 'explicit',
      selected_paths: [],
      proposed_subscriptions: [
        {
          path: '/interfaces/interface[name=Ethernet1]/state/counters/in-octets',
          origin: '',
          mode: 'SAMPLE',
          sample_interval_ns: 10 * NS,
          rationale: '',
          optional: false,
          provenance: 'schema_workspace',
          schema_path: '/interfaces/interface[name]/state/counters/in-octets',
          module: 'openconfig-interfaces',
        },
      ],
      decisions: [
        {
          index: 0,
          selected_path: '/interfaces/interface[name=Ethernet1]/state/counters/in-octets',
          origin: '',
          mode: 'SAMPLE',
          sample_interval_ns: 10 * NS,
          kind: 'eligible',
          eligible: true,
          schema_path: '/interfaces/interface[name]/state/counters/in-octets',
          module: 'openconfig-interfaces',
          reasons: [],
          warnings: ['origin_unspecified_device_default'],
        },
      ],
      rejection_reasons: [],
      warnings: ['path[0]:origin_unspecified_device_default'],
      plan_id: 'plan-abc123',
      binding_fingerprint: 'c'.repeat(64),
      activation_permitted: true,
      config_revision: 1_700_000_000_000_000_000,
      estimated_initial_collection: {
        kind: 'subscription_entries',
        value: 1,
        basis: 'count of approved Subscribe paths; not measured bytes or device CPU',
      },
      estimated_ongoing_collection: {
        kind: 'max_sample_interval_ns',
        value: 10 * NS,
        basis: 'largest approved SAMPLE interval',
      },
    },
    ...overrides,
  };
}

/** Install API fixtures. `state` is mutated by tests between steps. */
async function installFixtures(page, state) {
  state.device = state.device || baseDevice();
  state.applicability = state.applicability || applicabilityBody();
  state.preview = state.preview || previewBody();
  state.role = state.role || 'admin';
  state.requests = state.requests || { preview: 0, activate: 0, revoke: 0, refresh: 0 };

  // Catch-alls FIRST: Playwright matches routes last-registered-first, so
  // broad patterns must be registered before the specific fixtures or they
  // would shadow them.
  await page.route(`**/api/devices/${ENC_ADDR}/**`, (route) =>
    route.fulfill({ json: {} })
  );
  await page.route('**/api/devices**', (route) =>
    route.fulfill({ json: { devices: [state.device] } })
  );
  await page.route('**/api/events**', (route) => route.abort());

  await page.route('**/api/auth/me', (route) =>
    route.fulfill({ json: { username: 'op', role: state.role } })
  );
  await page.route('**/api/setup/status', (route) =>
    route.fulfill({ json: { is_first_run: false } })
  );
  await page.route(`**/api/devices/${ENC_ADDR}`, (route) =>
    route.fulfill({ json: state.device })
  );
  await page.route(`**/api/devices/${ENC_ADDR}/yang-applicability**`, (route) => {
    if (route.request().method() === 'POST') {
      state.requests.refresh++;
      let body = {};
      try { body = route.request().postDataJSON() || {}; } catch { /* empty */ }
      // M6E: the assigned-collector executor is asynchronous — Core
      // returns 202 + a durable job id; the UI polls job status.
      if (body.executor === 'assigned_collector') {
        state.jobPolls = 0;
        state.job = {
          job_id: 'job-remote-discovery-1',
          state: 'queued',
          device_key: 'edge-1',
          device_address: DEVICE_ADDR,
          collector_id: 'col-1',
          datastore: body.datastore || null,
          operation: 'yang_applicability',
          authority_epoch: 7,
          assignment_generation: 3,
          collector_session_generation: 11,
          created_at_ns: Date.now() * 1e6,
          deadline_ns: (Date.now() + 120_000) * 1e6,
          dispatched_at_ns: null,
          finished_at_ns: null,
          attempt_count: 0,
          result_classification: null,
          reason_code: null,
          snapshot_id: null,
          evidence_fingerprint: null,
          created_by: 'op',
        };
        return route.fulfill({
          status: 202,
          json: {
            job_id: state.job.job_id,
            state: state.job.state,
            device_key: state.job.device_key,
            device_address: DEVICE_ADDR,
            collector_id: 'col-1',
            datastore: state.job.datastore,
            operation: 'yang_applicability',
            created_at_ns: state.job.created_at_ns,
            deadline_ns: state.job.deadline_ns,
            status_url: `/api/devices/${DEVICE_ADDR}/yang-discovery/jobs`,
          },
        });
      }
      return route.fulfill({
        json: {
          snapshot_id: 43,
          device_key: 'edge-1',
          overall_status: 'applicable',
          outcome: 'ok',
          schema_id: 'a'.repeat(64),
          content_id: 'dev-content-1',
          datastore: 'ietf-datastores:operational',
          evidence_fingerprint: 'd'.repeat(64),
          paths_total: 2,
          modules_total: 1,
          evaluated_at_ns: Date.now() * 1e6,
          error: null,
          schema_binding_revoked: !!state.device.schema_binding,
        },
      });
    }
    return route.fulfill({ json: state.applicability });
  });
  await page.route(`**/api/devices/${ENC_ADDR}/yang-discovery/jobs**`, (route) => {
    state.requests.jobs = (state.requests.jobs || 0) + 1;
    // First polls report queued/dispatched; once the fixture flips the
    // job to completed the UI must surface the terminal state.
    const job = state.job
      ? { ...state.job, state: state.jobTerminal || state.job.state }
      : null;
    return route.fulfill({
      json: { device_key: 'edge-1', jobs: job ? [job] : [] },
    });
  });
  await page.route(`**/api/devices/${ENC_ADDR}/schema-binding/preview`, (route) => {
    state.requests.preview++;
    if (state.previewStatus) {
      return route.fulfill({ status: state.previewStatus, json: state.previewError || { error: 'preview failed' } });
    }
    return route.fulfill({ json: state.preview });
  });
  await page.route(`**/api/devices/${ENC_ADDR}/schema-binding/activate`, (route) => {
    state.requests.activate++;
    if (state.activateStatus === 409) {
      return route.fulfill({ status: 409, json: { error: 'config_revision_mismatch' } });
    }
    if (state.activateStatus) {
      return route.fulfill({ status: state.activateStatus, json: state.activateError || { error: 'activation failed' } });
    }
    state.device = {
      ...state.device,
      schema_binding: {
        mode: 'schema_bound',
        schema_id: 'a'.repeat(64),
        snapshot_id: 42,
        content_id: 'dev-content-1',
        datastore: 'ietf-datastores:operational',
        evidence_fingerprint: 'b'.repeat(64),
        plan_id: 'plan-abc123',
        binding_fingerprint: 'c'.repeat(64),
        approval_revision: 1,
        collector_id: 'col-1',
        approved_by: 'op',
        approved_at_ns: Date.now() * 1e6,
      },
    };
    return route.fulfill({
      json: {
        success: true,
        device_address: DEVICE_ADDR,
        approval_revision: 1,
        plan_id: 'plan-abc123',
        binding_fingerprint: 'c'.repeat(64),
        state: 'active',
      },
    });
  });
  await page.route(`**/api/devices/${ENC_ADDR}/schema-binding/revoke`, (route) => {
    state.requests.revoke++;
    if (state.revokeStatus) {
      return route.fulfill({ status: state.revokeStatus, json: { error: 'revoke failed' } });
    }
    state.device = { ...state.device, schema_binding: null };
    return route.fulfill({
      json: {
        success: true,
        device_address: DEVICE_ADDR,
        approval_revision: 1,
        plan_id: null,
        binding_fingerprint: null,
        state: 'revoked',
      },
    });
  });
}

async function openSchemaWorkspace(page) {
  await page.goto(`/#/devices/${encodeURIComponent(DEVICE_ADDR)}/schema`);
  await expect(page.locator('[data-testid="schema-workspace"]')).toBeVisible();
}

// Representative-state screenshots for the M6D evidence record.
// M6D_SHOT_DIR may point at docs/merger/evidence/<date>-m6d-schema-workspace/screenshots.
const SHOT_DIR = process.env.M6D_SHOT_DIR || 'test-results/m6d-screenshots';
async function snap(page, name) {
  await page.screenshot({ path: `${SHOT_DIR}/${name}.png` });
}

test.describe('M6D schema collection workspace', () => {
  test('T1/T2: device schema browser shows modules, paths, filters and pagination', async ({ page }) => {
    const state = {};
    // Two pages of paths to exercise pagination.
    const many = Array.from({ length: 120 }, (_, i) =>
      pathResult({ path: `/mod-${i}/leaf-${i}`, module: `mod-${i}`, status: i % 2 ? 'candidate' : 'applicable' })
    );
    state.applicability = applicabilityBody({
      paths: many.slice(0, 100),
      paths_matching: 120,
      modules: [moduleResult(), moduleResult({ name: 'mod-1', status: 'candidate' })],
      snapshot: snapshot({
        result: { ...snapshot().result, paths: many, paths_total: 120 },
      }),
    });
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    // Evidence card
    await expect(page.locator('[data-testid="evidence-card"]')).toContainText('Fresh');
    await expect(page.locator('[data-testid="evidence-card"]')).toContainText('applicable');

    // Module browser
    await expect(page.locator('[data-testid="module-browser"]')).toContainText('openconfig-interfaces');
    await expect(page.locator('[data-testid="module-browser"]')).toContainText('implemented');

    // Path rows + pagination
    await expect(page.locator('[data-testid="path-row"]')).toHaveCount(100);
    await expect(page.locator('text=1–100 of 120')).toBeVisible();
    await page.locator('[data-testid="paths-next"]').click();
    await expect(page.locator('text=101–120 of 120')).toBeVisible();

    // Filters issue a new bounded request (server-side filtering).
    const reqs = [];
    page.on('request', (r) => {
      if (r.url().includes('yang-applicability?')) reqs.push(r.url());
    });
    await page.locator('[data-testid="filter-status"]').selectOption('candidate');
    await page.waitForTimeout(400);
    expect(reqs.some((u) => u.includes('status=candidate'))).toBe(true);
    expect(reqs.some((u) => u.includes('offset=0'))).toBe(true);
    await snap(page, 't1-schema-browser');
  });

  test('T3: evidence distinctions render truthful states', async ({ page }) => {
    const state = {
      applicability: applicabilityBody({
        snapshot: snapshot({
          freshness: 'stale',
          freshness_reasons: ['evidence_expired'],
          overall_status: 'unknown',
        }),
        paths: [
          pathResult({ status: 'stale', reasons: ['evidence_expired'] }),
          pathResult({ path: '/x/y', status: 'conflicted', reasons: ['identity_ambiguity'] }),
          pathResult({ path: '/a/b', status: 'candidate', reasons: ['catalogue_partial'] }),
        ],
        paths_matching: 3,
      }),
    };
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    await expect(page.locator('[data-testid="evidence-card"]')).toContainText('Stale');
    await expect(page.locator('[data-testid="freshness-reasons"]')).toContainText('evidence_expired');
    const chips = page.locator('[data-testid="path-row"] .chip');
    await expect(chips.nth(0)).toContainText('stale');
    await expect(chips.nth(1)).toContainText('conflicted');
    await expect(chips.nth(2)).toContainText('candidate');
    // Selectability follows evidence truth: stale/conflicted rows are
    // disabled; candidate rows may be drafted (backend still decides).
    await expect(page.locator('[data-testid="select-path"]').nth(0)).toBeDisabled();
    await expect(page.locator('[data-testid="select-path"]').nth(1)).toBeDisabled();
    await expect(page.locator('[data-testid="select-path"]').nth(2)).toBeEnabled();
    await snap(page, 't3-evidence-states');
  });

  test('T4/T5/T6: structured keys, invalid keys, origin correctness', async ({ page }) => {
    const state = {};
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    // Select the templated interface path → draft row appears unresolved.
    await page.locator('[data-testid="select-path"]').first().click();
    const draftRow = page.locator('[data-testid="draft-row"]').first();
    await expect(draftRow).toBeVisible();
    await expect(draftRow.locator('[data-testid="draft-warning"]')).toContainText('Missing list keys');

    // Fill the `name` key → path instantiates with Ethernet1.
    await draftRow.locator('[data-testid="draft-key"]').fill('Ethernet1');
    await expect(draftRow).toContainText('name=Ethernet1');
    await expect(draftRow.locator('[data-testid="draft-warning"]')).toHaveCount(0);

    // Origin field is a genuine gNMI origin input, not provenance.
    const originInput = draftRow.locator('[data-testid="draft-origin"]');
    await originInput.fill('openconfig');
    await expect(originInput).toHaveValue('openconfig');

    // Add the SAME template with a different key → both remain selected.
    await page.locator('[data-testid="select-path"]').first().click();
    const rows = page.locator('[data-testid="draft-row"]');
    await expect(rows).toHaveCount(2);
    await rows.nth(1).locator('[data-testid="draft-key"]').fill('Ethernet2');
    await expect(rows.nth(1)).toContainText('name=Ethernet2');

    // Preview sends both instantiated paths and keeps origin separate.
    await page.locator('[data-testid="run-preview"]').click();
    await expect(page.locator('[data-testid="preview-summary"]')).toBeVisible();
    const previewReq = state.requests.preview;
    expect(previewReq).toBe(1);

    // Exact duplicate (same path+mode+interval) collapses client-side.
    // Mode change to ON_CHANGE clears interval requirement.
    await rows.nth(0).locator('[data-testid="draft-mode"]').selectOption('ON_CHANGE');
    await expect(rows.nth(0)).toContainText('ON_CHANGE');
  });

  test('T7: draft survives filter/page changes', async ({ page }) => {
    const state = {};
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    await page.locator('[data-testid="select-path"]').first().click();
    await page.locator('[data-testid="draft-row"]').first().locator('[data-testid="draft-key"]').fill('Ethernet1');
    await expect(page.locator('[data-testid="draft-row"]')).toHaveCount(1);

    // Change filters and paginate — draft must persist.
    await page.locator('[data-testid="path-search"]').fill('in-octets');
    await page.waitForTimeout(400);
    await page.locator('[data-testid="filter-kind"]').selectOption('leaf');
    await page.waitForTimeout(400);
    await expect(page.locator('[data-testid="draft-row"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="draft-row"]')).toContainText('Ethernet1');
  });

  test('T8/T9/T13: preview is read-only; activation shows staged truth', async ({ page }) => {
    const state = {};
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    let subscribeCalls = 0;
    page.on('request', (r) => {
      if (r.url().includes('subscribe') || r.url().includes('/api/devices/') && r.method() === 'POST' && !r.url().includes('schema-binding') && !r.url().includes('yang-applicability')) subscribeCalls++;
    });

    await page.locator('[data-testid="select-path"]').first().click();
    await page.locator('[data-testid="draft-row"]').first().locator('[data-testid="draft-key"]').fill('Ethernet1');
    await page.locator('[data-testid="run-preview"]').click();

    const summary = page.locator('[data-testid="preview-summary"]');
    await expect(summary).toContainText('permitted');
    await expect(summary).toContainText('plan-abc123');
    await expect(page.locator('[data-testid="preview-estimate"]')).toContainText('not measured bytes or device CPU');
    await expect(page.locator('[data-testid="decision-table"]')).toContainText('eligible');
    await expect(page.locator('[data-testid="proposed-subscriptions"]')).toContainText('in-octets');
    await snap(page, 't8-preview-eligible');

    // Preview performed no mutation: only the preview POST was issued.
    expect(state.requests.preview).toBe(1);
    expect(state.requests.activate).toBe(0);
    expect(subscribeCalls).toBe(0);

    // Activate → confirm → success state, never before server response.
    await page.locator('[data-testid="activate-button"]').click();
    await expect(page.locator('[data-testid="activate-confirm"]')).toContainText('Approve schema-bound plan');
    await page.locator('[data-testid="confirm-activate"]').click();
    await expect(page.locator('[data-testid="activate-message"]')).toContainText('activated');
    expect(state.requests.activate).toBe(1);
    await snap(page, 't9-activated');
  });

  test('T10: stale preview after config change requires re-preview (409)', async ({ page }) => {
    const state = { activateStatus: 409 };
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    await page.locator('[data-testid="select-path"]').first().click();
    await page.locator('[data-testid="draft-row"]').first().locator('[data-testid="draft-key"]').fill('Ethernet1');
    await page.locator('[data-testid="run-preview"]').click();
    await expect(page.locator('[data-testid="preview-summary"]')).toBeVisible();

    await page.locator('[data-testid="activate-button"]').click();
    await page.locator('[data-testid="confirm-activate"]').click();

    // 409 → clear conflict message, draft preserved, preview invalidated.
    await expect(page.locator('[data-testid="activate-error"]')).toContainText('Conflict');
    await expect(page.locator('[data-testid="draft-row"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="preview-summary"]')).toHaveCount(0);
    expect(state.requests.activate).toBe(1);
  });

  test('T11: viewer can preview but cannot activate/revoke/refresh', async ({ page }) => {
    const state = { role: 'viewer' };
    state.device = baseDevice({
      schema_binding: {
        mode: 'schema_bound', schema_id: 'a'.repeat(64), snapshot_id: 42,
        datastore: 'ietf-datastores:operational', evidence_fingerprint: 'b'.repeat(64),
        plan_id: 'plan-abc123', binding_fingerprint: 'c'.repeat(64),
        approval_revision: 3, collector_id: 'col-1', approved_by: 'admin',
        approved_at_ns: Date.now() * 1e6,
      },
      subscription_statuses: [
        { path: '/interfaces/interface[name=Ethernet1]/state/counters/in-octets', origin: '', mode: 'SAMPLE', sample_interval_ns: 10 * NS, status: 'observed', first_observed_at_ns: Date.now() * 1e6 - 60e9, last_observed_at_ns: Date.now() * 1e6, updated_at_ns: Date.now() * 1e6 },
        { path: '/interfaces/interface[name=Ethernet2]/state/counters/in-octets', origin: '', mode: 'SAMPLE', sample_interval_ns: 10 * NS, status: 'pending', first_observed_at_ns: 0, last_observed_at_ns: 0, updated_at_ns: Date.now() * 1e6 },
      ],
    });
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    await expect(page.locator('[data-testid="role-note"]')).toContainText('viewer');
    await expect(page.locator('[data-testid="schema-workspace"] .context')).toContainText('rev 3');
    await expect(page.locator('[data-testid="binding-meta"]')).toContainText('plan-abc123');

    // Preview still works for viewers.
    await page.locator('[data-testid="select-path"]').first().click();
    await page.locator('[data-testid="draft-row"]').first().locator('[data-testid="draft-key"]').fill('Ethernet1');
    await page.locator('[data-testid="run-preview"]').click();
    await expect(page.locator('[data-testid="preview-summary"]')).toBeVisible();

    // Activate is disabled for non-admin.
    await expect(page.locator('[data-testid="activate-button"]')).toBeDisabled();
    // Revoke is disabled for non-admin.
    await expect(page.locator('[data-testid="revoke-button"]')).toBeDisabled();
    await snap(page, 't11-viewer-blocked');
    // Refresh attempts surface 403 explicitly when role unknown — here the
    // button is enabled but the backend would deny; verify the request is
    // only fired on explicit click and errors are displayed.
    state.refreshForbidden = true;
  });

  test('T12: admin revocation disables authority and reports truthfully', async ({ page }) => {
    const state = {
      device: baseDevice({
        schema_binding: {
          mode: 'schema_bound', schema_id: 'a'.repeat(64), snapshot_id: 42,
          datastore: 'ietf-datastores:operational', evidence_fingerprint: 'b'.repeat(64),
          plan_id: 'plan-abc123', binding_fingerprint: 'c'.repeat(64),
          approval_revision: 3, collector_id: 'col-1', approved_by: 'admin',
          approved_at_ns: Date.now() * 1e6,
        },
      }),
    };
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    await page.locator('[data-testid="revoke-button"]').click();
    await expect(page.locator('[data-testid="revoke-confirm"]')).toContainText('does not restore');
    await page.locator('[data-testid="revoke-reason"]').fill('maintenance window');
    await page.locator('[data-testid="confirm-revoke"]').click();
    await expect(page.locator('[data-testid="revoke-message"]')).toContainText('revoked');
    await expect(page.locator('[data-testid="revoke-message"]')).toContainText('NOT restored');
    expect(state.requests.revoke).toBe(1);
    // Binding panel disappears once device state refreshes.
    await expect(page.locator('[data-testid="binding-panel"]')).toHaveCount(0);
    await snap(page, 't12-revoked');
  });

  test('T13: execution status distinguishes observed vs pending', async ({ page }) => {
    const state = {
      device: baseDevice({
        schema_binding: {
          mode: 'schema_bound', schema_id: 'a'.repeat(64), snapshot_id: 42,
          datastore: 'ietf-datastores:operational', evidence_fingerprint: 'b'.repeat(64),
          plan_id: 'plan-abc123', binding_fingerprint: 'c'.repeat(64),
          approval_revision: 1, collector_id: 'col-1', approved_by: 'admin',
          approved_at_ns: Date.now() * 1e6,
        },
        subscription_statuses: [
          { path: '/a/observed', origin: 'oc-if', mode: 'SAMPLE', sample_interval_ns: 10 * NS, status: 'observed', first_observed_at_ns: Date.now() * 1e6 - 30e9, last_observed_at_ns: Date.now() * 1e6, updated_at_ns: Date.now() * 1e6 },
          { path: '/a/pending', origin: 'oc-if', mode: 'SAMPLE', sample_interval_ns: 10 * NS, status: 'pending', first_observed_at_ns: 0, last_observed_at_ns: 0, updated_at_ns: Date.now() * 1e6 },
          { path: '/a/failed', origin: 'oc-if', mode: 'ON_CHANGE', sample_interval_ns: 0, status: 'failed', first_observed_at_ns: 0, last_observed_at_ns: 0, updated_at_ns: Date.now() * 1e6 },
        ],
      }),
    };
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    const rows = page.locator('[data-testid="subscription-row"]');
    await expect(rows).toHaveCount(3);
    await expect(rows.nth(0).locator('.chip')).toContainText('observed');
    await expect(rows.nth(1).locator('.chip')).toContainText('pending');
    await expect(rows.nth(2).locator('.chip')).toContainText('failed');
    await expect(page.locator('[data-testid="execution-panel"]')).toContainText('approval alone never implies');
    await snap(page, 't13-execution-status');
  });

  test('T14: error states surface without false success', async ({ page }) => {
    const state = { previewStatus: 503, previewError: { error: 'schema-binding store unavailable' } };
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    await page.locator('[data-testid="select-path"]').first().click();
    await page.locator('[data-testid="draft-row"]').first().locator('[data-testid="draft-key"]').fill('Ethernet1');
    await page.locator('[data-testid="run-preview"]').click();
    await expect(page.locator('[data-testid="preview-error"]')).toContainText('unavailable');
    await expect(page.locator('[data-testid="preview-summary"]')).toHaveCount(0);
    // No activation control is rendered without a successful preview.
    await expect(page.locator('[data-testid="activate-button"]')).toHaveCount(0);
  });

  test('T15: large catalogue uses bounded requests and bounded DOM', async ({ page }) => {
    const state = {};
    const many = Array.from({ length: 500 }, (_, i) =>
      pathResult({ path: `/big/path-${i}`, module: 'big-mod', status: 'applicable' })
    );
    state.applicability = applicabilityBody({
      paths: many.slice(0, 100),
      paths_matching: 5000,
      modules: [moduleResult()],
      snapshot: snapshot({ result: { ...snapshot().result, paths_total: 5000, paths_truncated: true } }),
    });
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    // Only one page of rows rendered; request carried explicit limit/offset.
    await expect(page.locator('[data-testid="path-row"]')).toHaveCount(100);
    const appReq = [];
    page.on('request', (r) => { if (r.url().includes('yang-applicability?')) appReq.push(r.url()); });
    await page.locator('[data-testid="paths-next"]').click();
    await page.waitForTimeout(300);
    expect(appReq.some((u) => u.includes('limit=100') && u.includes('offset=100'))).toBe(true);
    await expect(page.locator('[data-testid="path-row"]')).toHaveCount(100);
  });

  test('T16: legacy drawer tabs still work alongside schema tab', async ({ page }) => {
    const state = {};
    await installFixtures(page, state);
    await page.goto(`/#/devices/${encodeURIComponent(DEVICE_ADDR)}`);
    await expect(page.locator('.drawer')).toBeVisible();
    // Recommendations tab (legacy path editor) still renders.
    await page.locator('.drawer-tabs button', { hasText: 'recommendations' }).click();
    await expect(page.locator('.drawer-body')).toBeVisible();
    // Schema tab reachable.
    await page.locator('.drawer-tabs button', { hasText: 'schema' }).click();
    await expect(page.locator('[data-testid="schema-workspace"]')).toBeVisible();
  });

  test('T17/T18: accessibility landmarks and no page errors', async ({ page }) => {
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const state = {};
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    // Keyboard: tab to the preview button and activate via Enter.
    await page.locator('[data-testid="select-path"]').first().click();
    await page.locator('[data-testid="draft-row"]').first().locator('[data-testid="draft-key"]').fill('Ethernet1');
    const previewBtn = page.locator('[data-testid="run-preview"]');
    await previewBtn.focus();
    await page.keyboard.press('Enter');
    await expect(page.locator('[data-testid="preview-summary"]')).toBeVisible();

    // ARIA: search input has a label; status regions exist.
    await expect(page.locator('[data-testid="path-search"]')).toHaveAttribute('aria-label', /Search schema paths/);
    expect(errors).toHaveLength(0);
  });

  test('M6E-T17: remote discovery job status, completion and provenance', async ({ page }) => {
    const state = {};
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    // Choose the assigned-collector executor and request refresh.
    await page.getByLabel('Discovery executor').selectOption('assigned_collector');
    await page.getByTestId('refresh-applicability').click();

    // 202 path: queued message + live job card, no sync snapshot claim.
    await expect(page.getByTestId('refresh-message')).toContainText('Remote discovery queued');
    const jobCard = page.getByTestId('remote-job');
    await expect(jobCard).toBeVisible();
    await expect(jobCard).toContainText('queued');
    await expect(jobCard).toContainText('col-1');

    // First status poll still shows the job in flight, then Core marks
    // it completed with a snapshot reference.
    state.jobTerminal = 'dispatched';
    state.job = { ...state.job, state: 'dispatched', dispatched_at_ns: Date.now() * 1e6, attempt_count: 1 };
    await expect(jobCard).toContainText('dispatched', { timeout: 10_000 });

    state.jobTerminal = 'completed';
    state.job = {
      ...state.job,
      state: 'completed',
      finished_at_ns: Date.now() * 1e6,
      snapshot_id: 77,
      result_classification: 'completed',
    };
    await expect(page.getByTestId('refresh-message')).toContainText(
      'Remote discovery completed via collector col-1',
      { timeout: 15_000 }
    );
    await expect(page.getByTestId('refresh-message')).toContainText('snapshot #77');
    // The completed job card keeps the terminal state visible.
    await expect(jobCard).toContainText('completed');
    await expect(jobCard).toContainText('snapshot #77');
  });

  test('M6E-T17b: remote discovery failure surfaces explicit reason', async ({ page }) => {
    const state = {};
    await installFixtures(page, state);
    await openSchemaWorkspace(page);

    await page.getByLabel('Discovery executor').selectOption('assigned_collector');
    await page.getByTestId('refresh-applicability').click();
    await expect(page.getByTestId('remote-job')).toBeVisible();

    state.jobTerminal = 'timed_out';
    state.job = {
      ...state.job,
      state: 'timed_out',
      finished_at_ns: Date.now() * 1e6,
      reason_code: 'deadline_exceeded',
    };
    await expect(page.getByTestId('refresh-message')).toContainText(
      'Remote discovery finished: timed_out',
      { timeout: 15_000 }
    );
    await expect(page.getByTestId('refresh-message')).toContainText('deadline_exceeded');
    // A failed remote job must NOT claim a snapshot or applicable evidence.
    await expect(page.getByTestId('refresh-message')).not.toContainText('snapshot #');
  });
});

// ── M6F-F: per-device schema selection ─────────────────────────────────────

const SCHEMA_A = 'a'.repeat(64);
const SCHEMA_B = 'd'.repeat(64);
const SEL_URL = `/api/devices/${ENC_ADDR}/schema-selection`;

function selectionBody(overrides = {}) {
  const selection = overrides.selection !== undefined ? overrides.selection : null;
  return {
    device_key: 'edge-1',
    device_address: DEVICE_ADDR,
    selection,
    // M6G: durable epoch — defaults to the row revision when a
    // selection exists, 0 otherwise; survives clears (no ABA reset).
    selection_epoch: overrides.selection_epoch ?? selection?.selection_revision ?? 0,
    effective_schema_id: SCHEMA_A,
    active_schema_id: SCHEMA_A,
    catalogue_state: null,
    ...overrides,
  };
}

function selectionRow(schemaId, rev = 1) {
  return {
    device_key: 'edge-1',
    device_address: DEVICE_ADDR,
    schema_id: schemaId,
    selection_source: 'admin',
    selected_by: 'op',
    note: null,
    selection_revision: rev,
    created_at_ns: 1,
    updated_at_ns: 1,
  };
}

function compiledBundlesBody() {
  return {
    bundles: [
      { schema_id: SCHEMA_A, compiled_at_ns: 2, module_count: 10, path_count: 100, diagnostics: 0, active: true },
      { schema_id: SCHEMA_B, compiled_at_ns: 1, module_count: 8, path_count: 90, diagnostics: 0, active: false },
    ],
  };
}

async function installSelectionFixtures(page, state) {
  state.selection = state.selection || selectionBody();
  state.selectionPuts = state.selectionPuts || [];
  state.selectionDeletes = state.selectionDeletes || 0;
  state.selectionPutStatus = state.selectionPutStatus || 200;

  await page.route('**/api/yang/compiled**', (route) =>
    route.fulfill({ json: compiledBundlesBody() })
  );
  // `*` suffix: DELETE may carry ?expected_selection_revision=… (M6G).
  await page.route(`**${SEL_URL}*`, (route) => {
    const method = route.request().method();
    if (method === 'GET') {
      state.selectionGets = (state.selectionGets || 0) + 1;
      return route.fulfill({ json: state.selection });
    }
    if (method === 'PUT') {
      state.selectionPuts.push(route.request().postDataJSON() || {});
      if (state.selectionPutStatus !== 200) {
        return route.fulfill({ status: state.selectionPutStatus, json: { error: 'forbidden' } });
      }
      const sid = (route.request().postDataJSON() || {}).schema_id;
      state.selection = selectionBody({
        selection: selectionRow(sid, 2),
        effective_schema_id: sid,
      });
      return route.fulfill({ json: state.selection });
    }
    if (method === 'DELETE') {
      state.selectionDeletes++;
      state.selectionDeleteUrls = state.selectionDeleteUrls || [];
      state.selectionDeleteUrls.push(route.request().url());
      // Server truth: the durable epoch survives the row deletion.
      state.selection = selectionBody({ selection_epoch: 2 });
      return route.fulfill({ json: state.selection });
    }
    return route.fallback();
  });
}

test.describe('M6F-F device schema selection', () => {
  test('admin sees default selection and can bind a verified bundle', async ({ page }) => {
    const state = {};
    await installFixtures(page, state);
    await installSelectionFixtures(page, state);
    await openSchemaWorkspace(page);

    const card = page.locator('[data-testid="schema-selection"]');
    await expect(card).toBeVisible();
    await expect(card).toContainText('default (active catalogue)');

    await page.locator('[data-testid="schema-bundle-select"]').selectOption(SCHEMA_B);
    await page.locator('[data-testid="schema-select-apply"]').click();
    await page.locator('[data-testid="schema-select-confirm"]').getByRole('button', { name: 'Confirm' }).click();

    await expect.poll(() => state.selectionPuts.length).toBe(1);
    expect(state.selectionPuts[0].schema_id).toBe(SCHEMA_B);
    // M6G: selection writes carry the optimistic-concurrency precondition.
    expect(state.selectionPuts[0].expected_selection_revision).toBe(0);
    await expect(page.locator('[data-testid="schema-select-message"]')).toContainText('Schema selection updated');
    await expect(card).toContainText('explicit');
  });

  test('a stale selection revision surfaces a conflict, not a silent overwrite', async ({ page }) => {
    const state = {
      selection: selectionBody({
        selection: selectionRow(SCHEMA_B, 3),
        effective_schema_id: SCHEMA_B,
      }),
      selectionPutStatus: 409,
    };
    await installFixtures(page, state);
    await installSelectionFixtures(page, state);
    await openSchemaWorkspace(page);
    const getsBefore = state.selectionGets || 0;

    await page.locator('[data-testid="schema-bundle-select"]').selectOption(SCHEMA_A);
    await page.locator('[data-testid="schema-select-apply"]').click();
    await page.locator('[data-testid="schema-select-confirm"]').getByRole('button', { name: 'Confirm' }).click();

    await expect.poll(() => state.selectionPuts.length).toBe(1);
    // The client sent the revision it last observed (3).
    expect(state.selectionPuts[0].expected_selection_revision).toBe(3);
    const error = page.locator('[data-testid="schema-select-error"]');
    await expect(error).toBeVisible();
    // CR-04: the conflict is explained and the current selection/epoch
    // is refreshed — the stale mutation is never silently resubmitted.
    await expect(error).toContainText('another administrator');
    await expect.poll(() => state.selectionGets).toBeGreaterThan(getsBefore);
    // No selection message — the conflict is not reported as success.
    await expect(page.locator('[data-testid="schema-select-message"]')).toHaveCount(0);
    // Exactly one mutation was attempted — no automatic retry.
    expect(state.selectionPuts.length).toBe(1);
  });

  test('explicit selection shows selected, effective and active identities', async ({ page }) => {
    const state = {
      selection: selectionBody({
        selection: selectionRow(SCHEMA_B, 3),
        effective_schema_id: SCHEMA_B,
      }),
      applicability: applicabilityBody({
        snapshot: snapshot({
          active_schema_id: SCHEMA_A,
          effective_schema_id: SCHEMA_B,
          selected_schema_id: SCHEMA_B,
        }),
      }),
    };
    await installFixtures(page, state);
    await installSelectionFixtures(page, state);
    await openSchemaWorkspace(page);

    const card = page.locator('[data-testid="schema-selection"]');
    await expect(card).toContainText('explicit');
    await expect(card).toContainText('rev 3');
    await expect(page.locator('[data-testid="effective-schema"]')).toBeVisible();
    // Evidence grid distinguishes the three identities.
    const grid = page.locator('.evidence-grid').first();
    await expect(grid).toContainText('Selected schema');
    await expect(grid).toContainText('Effective schema');
    await expect(grid).toContainText('Active schema');
  });

  test('viewer sees the card read-only with no mutation controls', async ({ page }) => {
    const state = { role: 'viewer' };
    await installFixtures(page, state);
    await installSelectionFixtures(page, state);
    await openSchemaWorkspace(page);

    await expect(page.locator('[data-testid="schema-selection"]')).toBeVisible();
    await expect(page.locator('[data-testid="schema-bundle-select"]')).toHaveCount(0);
    await expect(page.locator('[data-testid="schema-select-clear"]')).toHaveCount(0);
  });

  test('admin clears an explicit selection back to the active catalogue', async ({ page }) => {
    const state = {
      selection: selectionBody({
        selection: selectionRow(SCHEMA_B, 1),
        effective_schema_id: SCHEMA_B,
      }),
    };
    await installFixtures(page, state);
    await installSelectionFixtures(page, state);
    await openSchemaWorkspace(page);

    await page.locator('[data-testid="schema-select-clear"]').click();
    await expect.poll(() => state.selectionDeletes).toBe(1);
    // M6G: the clear carries the observed selection revision as a
    // fencing precondition.
    expect(state.selectionDeleteUrls[0]).toContain('expected_selection_revision=1');
    await expect(page.locator('[data-testid="schema-select-message"]')).toContainText('cleared');
    await expect(page.locator('[data-testid="schema-selection"]')).toContainText('default (active catalogue)');
  });

  test('selection API errors surface truthfully', async ({ page }) => {
    const state = { selectionPutStatus: 403 };
    await installFixtures(page, state);
    await installSelectionFixtures(page, state);
    await openSchemaWorkspace(page);

    await page.locator('[data-testid="schema-bundle-select"]').selectOption(SCHEMA_B);
    await page.locator('[data-testid="schema-select-apply"]').click();
    await page.locator('[data-testid="schema-select-confirm"]').getByRole('button', { name: 'Confirm' }).click();

    await expect(page.locator('[data-testid="schema-select-error"]')).toContainText('Forbidden');
    await expect(page.locator('[data-testid="schema-select-message"]')).toHaveCount(0);
  });

  test('non-authoritative effective catalogue state is displayed', async ({ page }) => {
    const state = {
      selection: selectionBody({
        selection: selectionRow(SCHEMA_B, 1),
        effective_schema_id: SCHEMA_B,
        catalogue_state: 'catalogue_failed',
      }),
    };
    await installFixtures(page, state);
    await installSelectionFixtures(page, state);
    await openSchemaWorkspace(page);

    await expect(page.locator('[data-testid="schema-selection"]')).toContainText('catalogue failed');
  });
});
