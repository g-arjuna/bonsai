// @ts-check
// BN-024 browser acceptance for operational LLDP lifecycle presentation.
// The real built UI is driven in Chromium; API responses are route fixtures
// shaped like the authorized backend contract. Lifecycle fencing, monotonic
// ordering, withdrawal semantics and projection ownership are covered by the
// Rust integration tests in tests/bn024_lldp_operational.rs — this spec
// verifies the operator-facing presentation of those states only.
import { test, expect } from '@playwright/test';

const DEVICE_ADDR = '172.20.0.21';
const ENC_ADDR = encodeURIComponent(DEVICE_ADDR);
const NOW_NS = 1_700_000_000_000_000_000;

function lldpNeighbor(overrides) {
  return {
    local_if: 'Ethernet1',
    system_name: 'leaf1',
    port_id: 'Ethernet1',
    chassis_id: 'aa:leaf:01',
    neighbor_key: 'leaf1:proto:lldp:Ethernet1:aa:leaf:01',
    lifecycle: 'active',
    updated_at_ns: NOW_NS,
    observed_at_ns: NOW_NS,
    lifecycle_changed_at_ns: NOW_NS,
    ordering_state: 'ordered',
    ...overrides,
  };
}

function baseDevice(lldpNeighbors, omittedByLimit) {
  return {
    address: DEVICE_ADDR,
    hostname: 'ceos1',
    vendor: 'arista',
    role: 'leaf',
    site: 'lab',
    enabled: true,
    collector_id: null,
    health: 'unknown',
    acquisition: 'explicit',
    acquisition_plan_state: null,
    acquisition_plan_id: null,
    schema_binding: null,
    updated_at_ns: NOW_NS,
    updated_by: 'test',
    external_id: 'containerlab:ceos1',
    selected_paths: [],
    subscription_statuses: [],
    interfaces: [],
    bgp_neighbors: [],
    lldp_neighbors: lldpNeighbors,
    lldp_evidence_returned: lldpNeighbors.length,
    lldp_evidence_omitted_by_limit: omittedByLimit,
    lldp_evidence_contract:
      'retained lifecycle evidence, bounded to 500 rows; positive-empty collection coverage is not established',
    recent_state_changes: [],
    recent_detections: [],
    resolution_audit: [],
    created_at_ns: 1,
    created_by: 'test',
    last_operator_action: '',
  };
}

async function installFixtures(page, device) {
  await page.route(`**/api/devices/${ENC_ADDR}/**`, (route) =>
    route.fulfill({ json: {} })
  );
  await page.route('**/api/devices**', (route) =>
    route.fulfill({ json: { devices: [device] } })
  );
  await page.route('**/api/events**', (route) => route.abort());
  await page.route('**/api/auth/me', (route) =>
    route.fulfill({ json: { username: 'browser-op', role: 'operator' } })
  );
  await page.route('**/api/setup/status', (route) =>
    route.fulfill({ json: { is_first_run: false } })
  );
  await page.route(`**/api/devices/${ENC_ADDR}`, (route) =>
    route.fulfill({ json: device })
  );
}

async function openPeersTab(page) {
  await page.goto(`/#/devices/${ENC_ADDR}`);
  await page.getByRole('button', { name: 'peers', exact: true }).click();
}

test('operator sees distinct active, withdrawn and superseded lifecycle states', async ({
  page,
}) => {
  const device = baseDevice(
    [
      lldpNeighbor({ local_if: 'Ethernet1', system_name: 'leaf1', lifecycle: 'active' }),
      lldpNeighbor({
        local_if: 'Ethernet2',
        system_name: 'leaf2',
        chassis_id: 'aa:leaf:02',
        neighbor_key: 'leaf2:proto:lldp:Ethernet2:aa:leaf:02',
        lifecycle: 'withdrawn',
      }),
      lldpNeighbor({
        local_if: 'Ethernet3',
        system_name: 'leaf3',
        chassis_id: 'aa:leaf:03',
        neighbor_key: 'legacy:Ethernet3',
        lifecycle: 'superseded',
      }),
    ],
    false
  );
  await installFixtures(page, device);
  await openPeersTab(page);

  const activeRow = page.getByRole('row', { name: /leaf1/ });
  await expect(activeRow.locator('.badge', { hasText: 'active' })).toBeVisible();
  const withdrawnRow = page.getByRole('row', { name: /leaf2/ });
  await expect(withdrawnRow.locator('.badge', { hasText: 'withdrawn' })).toBeVisible();
  const supersededRow = page.getByRole('row', { name: /leaf3/ });
  await expect(supersededRow.locator('.badge', { hasText: 'superseded' })).toBeVisible();

  // Withdrawn/superseded evidence must not be presented as current adjacency
  // proof: the drawer states the contract explicitly and non-active rows
  // carry the non-healthy (critical) badge styling.
  await expect(page.locator('.drawer')).toContainText(
    'Withdrawn rows are historical evidence, not current adjacency proof'
  );
  await expect(withdrawnRow.locator('.badge.critical', { hasText: 'withdrawn' })).toHaveCount(1);
  await expect(supersededRow.locator('.badge.critical', { hasText: 'superseded' })).toHaveCount(1);
  await expect(activeRow.locator('.badge.healthy', { hasText: 'active' })).toHaveCount(1);
  // No ordering-uncertainty badge for ordered evidence.
  await expect(page.locator('.drawer')).not.toContainText('ordering uncertain');
});

test('ordering uncertainty is surfaced explicitly rather than hidden', async ({ page }) => {
  const device = baseDevice(
    [
      lldpNeighbor({
        lifecycle: 'active',
        ordering_state: 'conflicting',
      }),
    ],
    false
  );
  await installFixtures(page, device);
  await openPeersTab(page);
  const row = page.getByRole('row', { name: /leaf1/ });
  await expect(row).toContainText('ordering uncertain');
});

test('bounded evidence is disclosed when rows are omitted by the query limit', async ({
  page,
}) => {
  const device = baseDevice([lldpNeighbor({})], true);
  await installFixtures(page, device);
  await openPeersTab(page);
  await expect(page.locator('.drawer')).toContainText(
    'additional rows omitted by the 500-row device drawer limit'
  );
});

test('unbounded complete evidence does not claim omission', async ({ page }) => {
  const device = baseDevice([lldpNeighbor({})], false);
  await installFixtures(page, device);
  await openPeersTab(page);
  await expect(page.locator('.drawer')).toContainText('Retained lifecycle evidence');
  await expect(page.locator('.drawer')).not.toContainText('omitted by the 500-row');
});

test('empty peer evidence renders the explicit empty state, not phantom rows', async ({
  page,
}) => {
  const device = baseDevice([], false);
  await installFixtures(page, device);
  await openPeersTab(page);
  await expect(page.locator('.drawer')).toContainText('No peers recorded yet.');
  await expect(page.locator('.drawer')).not.toContainText('Retained lifecycle evidence');
});
