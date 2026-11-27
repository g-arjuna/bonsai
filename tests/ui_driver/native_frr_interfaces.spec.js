// @ts-check
// BN-024-04 browser acceptance for the bounded native FRR interface panel.
// The real built UI is driven in Chromium; API responses are route fixtures
// shaped like the authorized backend contract. Backend auth/inventory/SSH
// behavior is separately covered by Rust integration and live ignored tests.
import { test, expect } from '@playwright/test';

const DEVICE_ADDR = '172.100.208.12';
const ENC_ADDR = encodeURIComponent(DEVICE_ADDR);
const NOW_NS = BigInt(Date.now()) * 1_000_000n;
const SHA256 = 'b61b12df7142a9fcfacb4718eb9fc2b3210cd91a68122a87254dd093855c489a';

function fact(name, index, oper, admin, addresses, notes = []) {
  return {
    fact_id: `${name}-fact-0123456789abcdef`,
    fact_kind: 'interface.state',
    schema_id: 'interface_state/v1',
    subject: {
      device_id: 'containerlab:clab-bonsai-bn003-vrf-frr2',
      entity: name,
      namespace: 'interface',
      network_namespace: 'default',
    },
    payload: {
      state: oper === 'up' ? 'Valid' : 'Partial',
      fields: {
        ifindex: { state: 'known', value: index },
        interface_type: { state: 'known', value: 'physical' },
        ipv6_addresses: { state: 'known', value: [] },
        operational_state: { state: 'known', value: oper },
        administrative_state: { state: 'known', value: admin },
        ip_addresses: { state: 'known', value: addresses },
      },
    },
    quality: {
      completeness: 'complete',
      confidence: 'direct_observation',
      freshness: 'current',
      validation: 'validated',
      notes,
    },
    lifecycle: 'active',
    evidence: [],
  };
}


function macFact() {
  return {
    fact_id: 'mac-fact-0123456789abcdef',
    fact_kind: 'mac.forwarding',
    schema_id: 'mac_forwarding/v1',
    subject: {
      device_id: 'containerlab:clab-bonsai-bn003-vrf-frr2',
      entity: 'eth0',
      namespace: 'interface',
      network_namespace: 'default',
    },
    payload: {
      fields: {
        mac_address: '33:33:00:00:00:01',
        vlan_id: { state: 'unknown' },
        outgoing_interface: 'eth0',
        entry_type: { state: 'known', value: 'permanent' },
        flags: ['self', 'permanent'],
        move_count: { state: 'unknown' },
        last_move_epoch_seconds: { state: 'unknown' },
        bridge_domain: { state: 'unknown' },
        entry_contexts: ['self_table'],
      },
    },
    quality: {
      completeness: 'complete',
      confidence: 'direct_observation',
      freshness: 'current',
      validation: 'validated',
      notes: [],
    },
    lifecycle: 'active',
    evidence: [],
  };
}

function featureFamilies(withL2) {
  const coverage = (kind, positiveEmpty) => ({
    covered_fact_kind: kind,
    population_coverage: 'complete',
    positive_empty_population: positiveEmpty,
    capture_outcome: 'complete',
  });
  return [
    { feature: 'interfaces.frr_json', facts_emitted: 4, coverage: coverage('interface.state', false) },
    { feature: 'arpnd.linux_ip_neigh', facts_emitted: 0, coverage: coverage('arpnd.neighbor', true) },
    { feature: 'mac.linux_bridge_fdb', facts_emitted: withL2 ? 1 : 0, coverage: coverage('mac.forwarding', !withL2) },
    { feature: 'vlan.linux_bridge_vlan', facts_emitted: 0, coverage: coverage('vlan.bridge_domain', true) },
  ];
}

function evidenceBody(state) {
  return {
    device_address: DEVICE_ADDR,
    network_namespace: 'default',
    captured_at: new Date().toISOString(),
    captured_at_ns: Number(NOW_NS) - state.ageSeconds * 1_000_000_000,
    age_seconds: state.ageSeconds,
    stale_after_seconds: 3600,
    freshness: state.ageSeconds > 3600 ? 'stale' : 'fresh',
    release_raw: '10.6.1_git',
    release_build: 'quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b',
    acquisition_contract: 'frr_verified_ssh_l2_snapshot_v1',
    parser_revision: 'frr-iface-json-2026.11-r2',
    normalizer_revision: 'frr-iface-json-2026.11-r2',
    mapping_revision: 'bn024-iface-map-3',
    profile_id: 'frr-10.6.1-structured-interfaces',
    normalization_report: { entities_accepted: 4, records_decoded: 4, expected_interface_total: null, feature_families: featureFamilies(Boolean(state.withL2)) },
    transport: 'verified_ssh_read_only_cli',
    interface_command: "vtysh -c 'show interface json'",
    interface_output_bytes: 2796,
    interface_output_sha256: SHA256,
    host_key_verified: true,
    facts_emitted: state.withL2 ? 5 : 4,
    facts_validated: state.withL2 ? 5 : 4,
    facts_retained: state.withL2 ? 5 : 4,
    usable_entity_facts: state.withL2 ? 5 : 4,
    interpretation_outcome: 'complete_usable',
    fact_retention_truncated: false,
    records_decoded: 4,
    entities_encountered: 4,
    entities_accepted: 4,
    promotion: 'shadow_only',
    authoritative_projection: 'none',
    facts: [
      fact('eth0', 2, 'up', 'up', ['172.100.208.12/24']),
      fact('eth1', 3, 'up', 'up', [], ['no IPv4 unicast addresses observed in ipv4UnicastList']),
      fact('eth2', 4, 'down', 'up', []),
      fact('lo', 1, 'up', 'up', ['127.0.0.1/8']),
      ...(state.withL2 ? [macFact()] : []),
    ],
  };
}

function baseDevice() {
  return {
    address: DEVICE_ADDR,
    hostname: 'frr2',
    vendor: 'frr',
    role: 'router',
    site: 'lab',
    enabled: true,
    collector_id: null,
    health: 'unknown',
    acquisition: 'explicit',
    acquisition_plan_state: null,
    acquisition_plan_id: null,
    schema_binding: null,
    updated_at_ns: Number(NOW_NS),
    external_id: 'containerlab:clab-bonsai-bn003-vrf-frr2',
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
  };
}

async function installFixtures(page, state) {
  state.role = state.role || 'operator';
  state.ageSeconds = state.ageSeconds ?? 120;
  state.collects = 0;
  await page.route(`**/api/devices/${ENC_ADDR}/**`, (route) => route.fulfill({ json: {} }));
  await page.route('**/api/devices**', (route) =>
    route.fulfill({ json: { devices: [baseDevice()] } })
  );
  await page.route('**/api/events**', (route) => route.abort());
  await page.route('**/api/auth/me', (route) =>
    route.fulfill({ json: { username: 'browser-op', role: state.role } })
  );
  await page.route('**/api/setup/status', (route) =>
    route.fulfill({ json: { is_first_run: false } })
  );
  await page.route(`**/api/devices/${ENC_ADDR}`, (route) =>
    route.fulfill({ json: baseDevice() })
  );
  await page.route(`**/api/devices/${ENC_ADDR}/native-interfaces/frr/collect`, (route) => {
    state.collects++;
    state.ageSeconds = 0;
    return route.fulfill({
      json: {
        outcome: 'success',
        result: {
          device_address: DEVICE_ADDR,
          network_namespace: 'default',
          qualification: 'Qualified',
          shadow_only: true,
          authoritative_projection: 'none',
          facts_emitted: 4,
          facts_validated: 4,
          facts_retained: 4,
          usable_entity_facts: 4,
          interpretation_outcome: 'complete_usable',
          interface_command: "vtysh -c 'show interface json'",
          interface_output_sha256: SHA256,
        },
      },
    });
  });
  await page.route(`**/api/devices/${ENC_ADDR}/native-interfaces/frr`, (route) =>
    route.fulfill({
      json: state.acquisitionFailed
        ? {
            state: state.failedWithEvidence ? 'available' : 'acquisition_failed',
            evidence: state.failedWithEvidence ? evidenceBody(state) : null,
            freshness: 'stale',
            age_seconds: state.ageSeconds,
            stale_after_seconds: 3600,
            acquisition: {
              last_attempt_outcome: 'failed',
              last_failure_class: 'capture_transport_or_interpretation_failed',
              consecutive_failures: 2,
            },
            authoritative_projection: 'none',
            promotion: 'shadow_only',
          }
        : {
            evidence: evidenceBody(state),
            freshness: state.ageSeconds > 3600 ? 'stale' : 'fresh',
            age_seconds: state.ageSeconds,
            stale_after_seconds: 3600,
          },
    })
  );
}

async function openDrawer(page) {
  await page.goto(`/#/devices/${ENC_ADDR}`);
  await expect(page.getByRole('region', { name: 'Native FRR interface evidence' })).toBeVisible();
}

test('operator sees truthful native evidence identity, bounded facts, staleness, and safe collection action', async ({ page }) => {
  const state = { role: 'operator', ageSeconds: 3900 };
  await installFixtures(page, state);
  await openDrawer(page);
  const panel = page.getByRole('region', { name: 'Native FRR interface evidence' });
  await expect(panel).toContainText('Standalone FRR · shadow-only · no authoritative projection');
  await expect(panel).toContainText('Evidence is stale; collect a new authorized snapshot');
  await expect(panel).toContainText('frr-iface-json-2026.11-r2 / bn024-iface-map-3');
  await expect(panel).toContainText('verified against pinned known-hosts policy');
  await expect(panel).not.toContainText('SHA256:w/HyErTm6v1GknV7Mcae7K5xUMVnIVDvlFE5TfSoT1E');
  await expect(panel.getByRole('row', { name: /eth0/ })).toContainText('172.100.208.12/24');
  await expect(panel.getByRole('row', { name: /eth1/ })).toContainText('up');
  await expect(panel).toContainText('no IPv4 unicast addresses observed');
  await expect(panel).toContainText('4 interfaces observed');
  await expect(panel).toContainText('expected population unknown');
  await expect(panel).toContainText('4 validated entity facts');
  await expect(panel).not.toContainText('4/4 interfaces');
  await expect(panel).not.toContainText('password');

  await panel.getByRole('button', { name: 'Collect snapshot' }).click();
  await expect(panel).toContainText('Capture success: 4/4 usable/validated facts');
  expect(state.collects).toBe(1);
  await expect(panel).not.toContainText('Evidence is stale');
});


test('operator sees per-family L2 coverage and retained MAC evidence without inventing empty neighbor rows', async ({ page }) => {
  await installFixtures(page, { role: 'operator', ageSeconds: 120, withL2: true });
  await openDrawer(page);
  const panel = page.getByRole('region', { name: 'Native FRR interface evidence' });
  await expect(panel).toContainText('arpnd.linux_ip_neigh');
  await expect(panel).toContainText('positive empty population');
  await expect(panel).toContainText('L2 neighbor/bridge evidence');
  await expect(panel).toContainText('mac.forwarding');
  await expect(panel).toContainText('33:33:00:00:00:01 on eth0');
  await expect(panel).not.toContainText('No typed interface facts retained');
});

test('viewer receives an explicit denied affordance rather than a mutation action', async ({ page }) => {
  await installFixtures(page, { role: 'viewer', ageSeconds: 120 });
  await openDrawer(page);
  const panel = page.getByRole('region', { name: 'Native FRR interface evidence' });
  await expect(panel).toContainText('operator role required');
  await expect(panel.getByRole('button', { name: 'Collect snapshot' })).toHaveCount(0);
  await expect(panel.getByRole('row', { name: /lo/ })).toContainText('127.0.0.1/8');
});

test('failed acquisition remains explicit instead of rendering an empty successful interface table', async ({ page }) => {
  await installFixtures(page, { role: 'operator', acquisitionFailed: true });
  await openDrawer(page);
  const panel = page.getByRole('region', { name: 'Native FRR interface evidence' });
  await expect(panel).toContainText('Latest native acquisition failed');
  await expect(panel).toContainText('capture_transport_or_interpretation_failed');
  await expect(panel).not.toContainText('No interfaces recorded yet.');
  await expect(panel.getByRole('row', { name: /eth0/ })).toHaveCount(0);
});


test('failed acquisition preserves but clearly marks the last usable observation', async ({ page }) => {
  await installFixtures(page, { role: 'operator', acquisitionFailed: true, failedWithEvidence: true, ageSeconds: 3900 });
  await openDrawer(page);
  const panel = page.getByRole('region', { name: 'Native FRR interface evidence' });
  await expect(panel).toContainText('Latest native acquisition failed');
  await expect(panel).toContainText('showing the last usable shadow observation, not a refreshed result');
  await expect(panel.getByRole('row', { name: /eth0/ })).toContainText('172.100.208.12/24');
  await expect(panel).toContainText('Evidence is stale');
});

test('switching devices fences delayed native evidence so device A cannot overwrite device B', async ({ page }) => {
  const a = '172.100.208.12';
  const b = '172.100.208.13';
  const encA = encodeURIComponent(a);
  const encB = encodeURIComponent(b);
  const deviceA = { ...baseDevice(), address: a, hostname: 'frr-a' };
  const deviceB = { ...baseDevice(), address: b, hostname: 'frr-b', external_id: 'containerlab:frr-b' };
  const evidenceA = evidenceBody({ ageSeconds: 120 });
  evidenceA.device_address = a;
  evidenceA.facts[0].payload.fields.ip_addresses.value = ['10.200.0.1/24'];
  const evidenceB = evidenceBody({ ageSeconds: 120 });
  evidenceB.device_address = b;
  evidenceB.facts[0].payload.fields.ip_addresses.value = ['10.200.0.2/24'];

  await page.route('**/api/devices/**', (route) => route.fulfill({ json: {} }));
  await page.route('**/api/devices**', (route) =>
    route.fulfill({ json: { devices: [deviceA, deviceB] } })
  );
  await page.route('**/api/events**', (route) => route.abort());
  await page.route('**/api/auth/me', (route) =>
    route.fulfill({ json: { username: 'browser-op', role: 'operator' } })
  );
  await page.route('**/api/setup/status', (route) =>
    route.fulfill({ json: { is_first_run: false } })
  );
  await page.route(`**/api/devices/${encA}`, (route) => route.fulfill({ json: deviceA }));
  await page.route(`**/api/devices/${encB}`, (route) => route.fulfill({ json: deviceB }));
  await page.route(`**/api/devices/${encA}/native-interfaces/frr`, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 350));
    return route.fulfill({ json: { evidence: evidenceA, freshness: 'fresh', age_seconds: 120 } });
  });
  await page.route(`**/api/devices/${encB}/native-interfaces/frr`, (route) =>
    route.fulfill({ json: { evidence: evidenceB, freshness: 'fresh', age_seconds: 120 } })
  );

  await page.goto(`/#/devices/${encA}`);
  const panel = page.getByRole('region', { name: 'Native FRR interface evidence' });
  await expect(panel).toBeVisible();
  await page.waitForTimeout(75);
  await page.goto(`/#/devices/${encB}`);
  await expect(panel).toContainText('10.200.0.2/24');
  await page.waitForTimeout(425);
  await expect(panel).toContainText('10.200.0.2/24');
  await expect(panel).not.toContainText('10.200.0.1/24');
  await expect(panel).not.toContainText('frr-a');
});
