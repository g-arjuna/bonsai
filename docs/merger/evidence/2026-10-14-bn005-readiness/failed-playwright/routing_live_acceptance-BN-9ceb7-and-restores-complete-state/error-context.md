# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: routing_live_acceptance.spec.js >> BN-003 Live Routing & RIB Browser Acceptance Gate >> 8. Reconnect on session epoch 2 resolves transport gap and restores complete state
- Location: tests/ui_driver/routing_live_acceptance.spec.js:263:3

# Error details

```
Error: expect(received).toContain(expected) // indexOf

Expected substring: "complete"
Received string:    "Completeness: gap"

Call Log:
- Timeout 20000ms exceeded while waiting on the predicate
```

# Page snapshot

```yaml
- generic [ref=e2]:
  - link "Skip to main content" [ref=e3] [cursor=pointer]:
    - /url: "#main-content"
  - generic [ref=e4]:
    - complementary "Application navigation" [ref=e5]:
      - generic "Bonsai" [ref=e6]: 🌿 bonsai
      - navigation "Primary navigation" [ref=e7]:
        - group "Monitoring" [ref=e8]:
          - generic [ref=e9]: Monitor
          - link "Live Shortcut Command 1" [ref=e10] [cursor=pointer]:
            - /url: "#/"
            - generic [ref=e11]: ◉
            - text: Live
            - generic "Shortcut Command 1" [ref=e12]: ⌘1
          - link "Incidents Shortcut Command 2" [ref=e13] [cursor=pointer]:
            - /url: "#/incidents"
            - generic [ref=e14]: ⚠
            - text: Incidents
            - generic "Shortcut Command 2" [ref=e15]: ⌘2
          - link "Devices Shortcut Command 3" [ref=e16] [cursor=pointer]:
            - /url: "#/devices"
            - generic [ref=e17]: ⊡
            - text: Devices
            - generic "Shortcut Command 3" [ref=e18]: ⌘3
          - link "Operations Shortcut Command 4" [ref=e19] [cursor=pointer]:
            - /url: "#/operations"
            - generic [ref=e20]: ♡
            - text: Operations
            - generic "Shortcut Command 4" [ref=e21]: ⌘4
          - link "Collectors Shortcut Command 5" [ref=e22] [cursor=pointer]:
            - /url: "#/collectors"
            - generic [ref=e23]: ⇄
            - text: Collectors
            - generic "Shortcut Command 5" [ref=e24]: ⌘5
          - link "Routing & RIB" [ref=e25] [cursor=pointer]:
            - /url: "#/routing"
            - generic [ref=e26]: 🧭
            - text: Routing & RIB
        - separator [ref=e27]
        - group "Operations" [ref=e28]:
          - generic [ref=e29]: Operate
          - link "Integrations Shortcut Command 6" [ref=e30] [cursor=pointer]:
            - /url: "#/integrations"
            - generic [ref=e31]: ⇌
            - text: Integrations
            - generic "Shortcut Command 6" [ref=e32]: ⌘6
          - link "Approvals Shortcut Command 7" [ref=e33] [cursor=pointer]:
            - /url: "#/approvals"
            - generic [ref=e34]: ✓
            - text: Approvals
            - generic "Shortcut Command 7" [ref=e35]: ⌘7
          - link "Explorer Shortcut Command 8" [ref=e36] [cursor=pointer]:
            - /url: "#/explorer"
            - generic [ref=e37]: ⬡
            - text: Explorer
            - generic "Shortcut Command 8" [ref=e38]: ⌘8
          - link "Investigations Shortcut Command 9" [ref=e39] [cursor=pointer]:
            - /url: "#/investigations"
            - generic [ref=e40]: 🔍
            - text: Investigations
            - generic "Shortcut Command 9" [ref=e41]: ⌘9
        - separator [ref=e42]
        - group "Configuration" [ref=e43]:
          - generic [ref=e44]: Configure
          - link "HA" [ref=e45] [cursor=pointer]:
            - /url: "#/ha"
            - generic [ref=e46]: ⚡
            - text: HA
          - link "Environments" [ref=e47] [cursor=pointer]:
            - /url: "#/environments"
            - generic [ref=e48]: ⬡
            - text: Environments
          - link "Profiles" [ref=e49] [cursor=pointer]:
            - /url: "#/profiles"
            - generic [ref=e50]: 📋
            - text: Profiles
          - link "Sites" [ref=e51] [cursor=pointer]:
            - /url: "#/sites"
            - generic [ref=e52]: ◎
            - text: Sites
          - link "Credentials" [ref=e53] [cursor=pointer]:
            - /url: "#/credentials"
            - generic [ref=e54]: ⚿
            - text: Credentials
          - link "Audit" [ref=e55] [cursor=pointer]:
            - /url: "#/audit"
            - generic [ref=e56]: 📜
            - text: Audit
          - link "Syslog / Shun" [ref=e57] [cursor=pointer]:
            - /url: "#/syslog"
            - generic [ref=e58]: ⬇
            - text: Syslog / Shun
          - link "Database" [ref=e59] [cursor=pointer]:
            - /url: "#/db"
            - generic [ref=e60]: 🗄
            - text: Database
          - link "Governance" [ref=e61] [cursor=pointer]:
            - /url: "#/governance"
            - generic [ref=e62]: ⚖
            - text: Governance
          - link "SNMP" [ref=e63] [cursor=pointer]:
            - /url: "#/snmp"
            - generic [ref=e64]: 📡
            - text: SNMP
          - link "Sidecars" [ref=e65] [cursor=pointer]:
            - /url: "#/sidecars"
            - generic [ref=e66]: 🤖
            - text: Sidecars
          - link "Users &amp; Access" [ref=e67] [cursor=pointer]:
            - /url: "#/users"
            - generic [ref=e68]: 👤
            - text: Users &amp; Access
          - link "Config Library" [ref=e69] [cursor=pointer]:
            - /url: "#/config-library"
            - generic [ref=e70]: 📦
            - text: Config Library
          - link "Settings" [ref=e71] [cursor=pointer]:
            - /url: "#/settings"
            - generic [ref=e72]: ⚙
            - text: Settings
          - link "Endpoints" [ref=e73] [cursor=pointer]:
            - /url: "#/endpoints"
            - generic [ref=e74]: ⬡
            - text: Endpoints
          - link "Certificates" [ref=e75] [cursor=pointer]:
            - /url: "#/certs"
            - generic [ref=e76]: 🔒
            - text: Certificates
          - link "Stream Control" [ref=e77] [cursor=pointer]:
            - /url: "#/stream-control"
            - generic [ref=e78]: ⏻
            - text: Stream Control
      - generic [ref=e79]:
        - button "⌨ Search Ctrl+K" [ref=e80] [cursor=pointer]:
          - generic [ref=e81]: ⌨
          - text: Search
          - generic [ref=e82]: Ctrl+K
        - link "ML Console" [ref=e83] [cursor=pointer]:
          - /url: /bonpy/
          - generic [ref=e84]: 🤖
          - text: ML Console
        - group "Display density" [ref=e86]:
          - button "Comfortable" [ref=e87] [cursor=pointer]
          - button "Compact" [ref=e88] [cursor=pointer]
        - 'generic "status: ok build: 1791962248" [ref=e89]':
          - generic [ref=e91]: v0.2.0 · e4d8c448
    - main [ref=e92]:
      - generic [ref=e93]:
        - generic [ref=e94]:
          - generic [ref=e95]:
            - paragraph [ref=e96]: Canonical BMP / BGP Control Plane
            - heading "Routing & RIB Observatory" [level=2] [ref=e97]
          - generic [ref=e98]:
            - generic "Overall RIB Completeness State" [ref=e99]: "Completeness: gap"
            - generic "Durable RibEvent Journal Sequence" [ref=e100]: "Seq #51"
            - generic "Graph Summary Projection Lag" [ref=e101]: "Graph Lag: 0"
            - generic "Last Routing Health Snapshot Time" [ref=e102]: "Observed: 12:50:21 PM"
            - button "Refresh" [active] [ref=e103] [cursor=pointer]
        - status [ref=e104]:
          - strong [ref=e105]: "Data-Quality Notice (gap):"
          - text: One or more BMP speaker sessions experienced a transport disconnect without explicit BGP route withdrawals. Last-known routes are preserved with a transport-gap indicator.
        - region "Routing Summary" [ref=e106]:
          - generic [ref=e107]:
            - generic [ref=e108]: BMP Speakers
            - strong [ref=e109]: "2"
          - generic [ref=e110]:
            - generic [ref=e111]: BGP Peers Up
            - strong [ref=e112]: "4"
          - generic [ref=e113]:
            - generic [ref=e114]: Active RIB Routes
            - strong [ref=e115]: "5"
          - generic [ref=e116]:
            - generic [ref=e117]: Transport Gaps
            - strong [ref=e118]: "0"
        - generic [ref=e119]:
          - generic [ref=e120]:
            - heading "BMP Exporters (Speakers)" [level=3] [ref=e121]
            - table [ref=e122]:
              - rowgroup [ref=e123]:
                - row "Speaker IP SysName Epoch Peers (Up/Total) Routes Completeness" [ref=e124]:
                  - columnheader "Speaker IP" [ref=e125]
                  - columnheader "SysName" [ref=e126]
                  - columnheader "Epoch" [ref=e127]
                  - columnheader "Peers (Up/Total)" [ref=e128]
                  - columnheader "Routes" [ref=e129]
                  - columnheader "Completeness" [ref=e130]
              - rowgroup [ref=e131]:
                - row "172.100.207.11 frr1 2 3/3 4 complete_for_observed_view" [ref=e132]:
                  - cell "172.100.207.11" [ref=e133]:
                    - code [ref=e134]: 172.100.207.11
                  - cell "frr1" [ref=e135]
                  - cell "2" [ref=e136]
                  - cell "3/3" [ref=e137]
                  - cell "4" [ref=e138]
                  - cell "complete_for_observed_view" [ref=e139]:
                    - generic [ref=e140]: complete_for_observed_view
                - row "172.100.207.12 frr2 1 1/2 1 complete_for_observed_view" [ref=e141]:
                  - cell "172.100.207.12" [ref=e142]:
                    - code [ref=e143]: 172.100.207.12
                  - cell "frr2" [ref=e144]
                  - cell "1" [ref=e145]
                  - cell "1/2" [ref=e146]
                  - cell "1" [ref=e147]
                  - cell "complete_for_observed_view" [ref=e148]:
                    - generic [ref=e149]: complete_for_observed_view
          - generic [ref=e150]:
            - heading "BGP Peer / Instance Sessions" [level=3] [ref=e151]
            - paragraph [ref=e152]: No BGP peer sessions observed.
        - generic [ref=e153]:
          - generic [ref=e154]:
            - heading "Canonical RIB Explorer" [level=3] [ref=e155]
            - generic [ref=e156]:
              - combobox "Address Family" [ref=e157]:
                - option "IPv4 Unicast (1/1)" [selected]
                - option "IPv6 Unicast (2/1)"
                - option "VPNv4 L3VPN (1/128)"
                - option "VPNv6 L3VPN (2/128)"
                - option "L2VPN EVPN (25/70)"
                - option "BGP-LS (16388/71)"
              - combobox "RIB View" [ref=e158]:
                - option "All Views" [selected]
                - option "Adj-RIB-In (Pre-Policy)"
                - option "Adj-RIB-In (Post-Policy)"
                - option "Loc-RIB"
                - option "Loc-RIB (Filtered)"
                - option "Adj-RIB-Out (Pre-Policy)"
                - option "Adj-RIB-Out (Post-Policy)"
              - textbox "Speaker IP Filter" [ref=e159]:
                - /placeholder: Speaker IP
              - textbox "Peer IP Filter" [ref=e160]:
                - /placeholder: Peer IP
              - textbox "VRF RD Filter" [ref=e161]:
                - /placeholder: VRF RD (e.g. 0:0 or 65000:100)
              - textbox "Prefix Filter" [ref=e162]:
                - /placeholder: Prefix (e.g. 10.10.0.0/24)
              - button "Filter Routes" [ref=e163] [cursor=pointer]
          - table [ref=e164]:
            - rowgroup [ref=e165]:
              - row "Prefix View VRF / RD Speaker Peer (ASN) Next Hop AS Path LP / MED Communities State / Freshness History" [ref=e166]:
                - columnheader "Prefix" [ref=e167]
                - columnheader "View" [ref=e168]
                - columnheader "VRF / RD" [ref=e169]
                - columnheader "Speaker" [ref=e170]
                - columnheader "Peer (ASN)" [ref=e171]
                - columnheader "Next Hop" [ref=e172]
                - columnheader "AS Path" [ref=e173]
                - columnheader "LP / MED" [ref=e174]
                - columnheader "Communities" [ref=e175]
                - columnheader "State / Freshness" [ref=e176]
                - columnheader "History" [ref=e177]
            - rowgroup [ref=e178]:
              - row "10.99.1.0/24 AdjRibInPrePolicy 65000:100 172.100.207.11 10.100.1.1 (AS65002) 10.100.1.1 65002 100 / — — complete_for_observed_view Timeline" [ref=e179]:
                - cell "10.99.1.0/24" [ref=e180]:
                  - code [ref=e181]: 10.99.1.0/24
                - cell "AdjRibInPrePolicy" [ref=e182]:
                  - code [ref=e183]: AdjRibInPrePolicy
                - cell "65000:100" [ref=e184]:
                  - code [ref=e185]: 65000:100
                - cell "172.100.207.11" [ref=e186]:
                  - code [ref=e187]: 172.100.207.11
                - cell "10.100.1.1 (AS65002)" [ref=e188]:
                  - code [ref=e189]: 10.100.1.1
                  - text: (AS65002)
                - cell "10.100.1.1" [ref=e190]:
                  - code [ref=e191]: 10.100.1.1
                - cell "65002" [ref=e192]:
                  - code [ref=e193]: "65002"
                - cell "100 / —" [ref=e194]
                - cell "—" [ref=e195]:
                  - code [ref=e196]: —
                - cell "complete_for_observed_view" [ref=e197]:
                  - generic [ref=e198]: complete_for_observed_view
                - cell "Timeline" [ref=e199]:
                  - button "Timeline" [ref=e200] [cursor=pointer]
              - row "10.10.1.0/24 LocRib 0:0 172.100.207.11 0.0.0.0 (AS65001) 10.100.1.1 65001 65002 65002 65002 — / 150 — complete_for_observed_view Timeline" [ref=e201]:
                - cell "10.10.1.0/24" [ref=e202]:
                  - code [ref=e203]: 10.10.1.0/24
                - cell "LocRib" [ref=e204]:
                  - code [ref=e205]: LocRib
                - cell "0:0" [ref=e206]:
                  - code [ref=e207]: 0:0
                - cell "172.100.207.11" [ref=e208]:
                  - code [ref=e209]: 172.100.207.11
                - cell "0.0.0.0 (AS65001)" [ref=e210]:
                  - code [ref=e211]: 0.0.0.0
                  - text: (AS65001)
                - cell "10.100.1.1" [ref=e212]:
                  - code [ref=e213]: 10.100.1.1
                - cell "65001 65002 65002 65002" [ref=e214]:
                  - code [ref=e215]: 65001 65002 65002 65002
                - cell "— / 150" [ref=e216]
                - cell "—" [ref=e217]:
                  - code [ref=e218]: —
                - cell "complete_for_observed_view" [ref=e219]:
                  - generic [ref=e220]: complete_for_observed_view
                - cell "Timeline" [ref=e221]:
                  - button "Timeline" [ref=e222] [cursor=pointer]
              - row "10.10.1.0/24 AdjRibInPrePolicy 0:0 172.100.207.11 10.100.1.1 (AS65002) 10.100.1.1 65001 65002 65002 65002 — / 150 — complete_for_observed_view Timeline" [ref=e223]:
                - cell "10.10.1.0/24" [ref=e224]:
                  - code [ref=e225]: 10.10.1.0/24
                - cell "AdjRibInPrePolicy" [ref=e226]:
                  - code [ref=e227]: AdjRibInPrePolicy
                - cell "0:0" [ref=e228]:
                  - code [ref=e229]: 0:0
                - cell "172.100.207.11" [ref=e230]:
                  - code [ref=e231]: 172.100.207.11
                - cell "10.100.1.1 (AS65002)" [ref=e232]:
                  - code [ref=e233]: 10.100.1.1
                  - text: (AS65002)
                - cell "10.100.1.1" [ref=e234]:
                  - code [ref=e235]: 10.100.1.1
                - cell "65001 65002 65002 65002" [ref=e236]:
                  - code [ref=e237]: 65001 65002 65002 65002
                - cell "— / 150" [ref=e238]
                - cell "—" [ref=e239]:
                  - code [ref=e240]: —
                - cell "complete_for_observed_view" [ref=e241]:
                  - generic [ref=e242]: complete_for_observed_view
                - cell "Timeline" [ref=e243]:
                  - button "Timeline" [ref=e244] [cursor=pointer]
              - row "10.10.1.0/24 AdjRibInPostPolicy 0:0 172.100.207.11 10.100.1.1 (AS65002) 10.100.1.1 65001 65002 65002 65002 — / 150 — complete_for_observed_view Timeline" [ref=e245]:
                - cell "10.10.1.0/24" [ref=e246]:
                  - code [ref=e247]: 10.10.1.0/24
                - cell "AdjRibInPostPolicy" [ref=e248]:
                  - code [ref=e249]: AdjRibInPostPolicy
                - cell "0:0" [ref=e250]:
                  - code [ref=e251]: 0:0
                - cell "172.100.207.11" [ref=e252]:
                  - code [ref=e253]: 172.100.207.11
                - cell "10.100.1.1 (AS65002)" [ref=e254]:
                  - code [ref=e255]: 10.100.1.1
                  - text: (AS65002)
                - cell "10.100.1.1" [ref=e256]:
                  - code [ref=e257]: 10.100.1.1
                - cell "65001 65002 65002 65002" [ref=e258]:
                  - code [ref=e259]: 65001 65002 65002 65002
                - cell "— / 150" [ref=e260]
                - cell "—" [ref=e261]:
                  - code [ref=e262]: —
                - cell "complete_for_observed_view" [ref=e263]:
                  - generic [ref=e264]: complete_for_observed_view
                - cell "Timeline" [ref=e265]:
                  - button "Timeline" [ref=e266] [cursor=pointer]
              - row "10.10.1.0/24 LocRib 0:0 172.100.207.12 0.0.0.0 (AS65002) 0.0.0.0 65002 — / 0 — complete_for_observed_view Timeline" [ref=e267]:
                - cell "10.10.1.0/24" [ref=e268]:
                  - code [ref=e269]: 10.10.1.0/24
                - cell "LocRib" [ref=e270]:
                  - code [ref=e271]: LocRib
                - cell "0:0" [ref=e272]:
                  - code [ref=e273]: 0:0
                - cell "172.100.207.12" [ref=e274]:
                  - code [ref=e275]: 172.100.207.12
                - cell "0.0.0.0 (AS65002)" [ref=e276]:
                  - code [ref=e277]: 0.0.0.0
                  - text: (AS65002)
                - cell "0.0.0.0" [ref=e278]:
                  - code [ref=e279]: 0.0.0.0
                - cell "65002" [ref=e280]:
                  - code [ref=e281]: "65002"
                - cell "— / 0" [ref=e282]
                - cell "—" [ref=e283]:
                  - code [ref=e284]: —
                - cell "complete_for_observed_view" [ref=e285]:
                  - generic [ref=e286]: complete_for_observed_view
                - cell "Timeline" [ref=e287]:
                  - button "Timeline" [ref=e288] [cursor=pointer]
        - generic [ref=e289]:
          - generic [ref=e290]:
            - heading "Prefix Event History Timeline" [level=3] [ref=e291]
            - generic [ref=e292]:
              - textbox "Prefix History Query" [ref=e293]:
                - /placeholder: Inspect prefix history (e.g. 10.10.0.0/24)
              - button "Load Timeline" [ref=e294] [cursor=pointer]
          - paragraph [ref=e295]: Select a route or enter a prefix above to inspect announce/withdraw transitions and before/after attributes.
        - region "Missing-Prefix Investigator and Operator Adjudication" [ref=e296]:
          - generic [ref=e297]:
            - generic [ref=e298]:
              - heading "Missing-Prefix Investigator & Operator Adjudication (A09 / A10)" [level=3] [ref=e299]
              - paragraph [ref=e300]:
                - text: Separates observed BMP/BGP facts from ranked hypotheses, citations (
                - code [ref=e301]: observation_id
                - text: ), missing planes, and structured operator adjudication.
            - generic [ref=e302]:
              - textbox "Investigate Prefix" [ref=e303]:
                - /placeholder: Prefix (e.g. 203.0.113.0/24)
                - text: 203.0.113.0/24
              - textbox "Investigate VRF RD" [ref=e304]:
                - /placeholder: VRF RD (0:0)
                - text: 0:0
              - textbox "Investigate Speaker IP" [ref=e305]:
                - /placeholder: Speaker IP (optional)
              - button "Explain Prefix" [ref=e306] [cursor=pointer]
          - generic [ref=e307]:
            - generic [ref=e308]:
              - generic [ref=e309]: "Adjudicated RCA Precision:"
              - strong [ref=e310]: 0%
            - generic [ref=e311]:
              - generic [ref=e312]: "Verified Correct:"
              - strong [ref=e313]: "0"
            - generic [ref=e314]:
              - generic [ref=e315]: "Verified Incorrect:"
              - strong [ref=e316]: "0"
            - generic [ref=e317]:
              - generic [ref=e318]: "Accepted w/o RCA Proof:"
              - strong [ref=e319]: "0"
            - generic [ref=e320]:
              - generic [ref=e321]: "Training-Eligible:"
              - strong [ref=e322]: "0"
          - paragraph [ref=e323]:
            - text: Enter a prefix above and click
            - strong [ref=e324]: Explain Prefix
            - text: to inspect grounded control-plane facts, ranked hypotheses, citations, and operator adjudication controls.
        - generic [ref=e325]:
          - heading "AFI / SAFI Protocol Coverage Matrix" [level=3] [ref=e326]
          - generic [ref=e327]:
            - generic [ref=e328]:
              - strong [ref=e329]: ipv4-unicast
              - generic [ref=e330]: (1/1)
              - generic [ref=e331]: supported
            - generic [ref=e332]:
              - strong [ref=e333]: ipv6-unicast
              - generic [ref=e334]: (2/1)
              - generic [ref=e335]: supported
            - generic [ref=e336]:
              - strong [ref=e337]: ipv4-vpn
              - generic [ref=e338]: (1/128)
              - generic [ref=e339]: supported
            - generic [ref=e340]:
              - strong [ref=e341]: ipv6-vpn
              - generic [ref=e342]: (2/128)
              - generic [ref=e343]: supported
            - generic [ref=e344]:
              - strong [ref=e345]: l2vpn-evpn
              - generic [ref=e346]: (25/70)
              - generic [ref=e347]: experimental_parse_only
            - generic [ref=e348]:
              - strong [ref=e349]: bgp-ls
              - generic [ref=e350]: (16388/71)
              - generic [ref=e351]: experimental_parse_only
```

# Test source

```ts
  177 |     // Trigger route-map on frr2 via control server
  178 |     const modRes = await request.post(`${CTRL_URL}/frr/modify`);
  179 |     expect(modRes.status()).toBe(200);
  180 | 
  181 |     await page.goto(`${BONSAI_URL}/#/routing`);
  182 |     await page.waitForLoadState('domcontentloaded');
  183 | 
  184 |     const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
  185 |     const ribTable = page.locator('.panel', { hasText: 'Canonical RIB Explorer' }).locator('.data-table');
  186 | 
  187 |     // Poll Filter button until updated AS-path or MED is rendered
  188 |     await expect(async () => {
  189 |       await filterBtn.click();
  190 |       await expect(ribTable).toContainText('65002 65002', { timeout: 2000 });
  191 |     }).toPass({ timeout: 20_000, intervals: [1000] });
  192 | 
  193 |     // Also verify MED metric 150
  194 |     await expect(ribTable).toContainText('150');
  195 | 
  196 |     await page.screenshot({ path: path.join(EVIDENCE_DIR, '02_route_attribute_modified.png'), fullPage: true });
  197 |   });
  198 | 
  199 |   test('6. Route withdrawal removes prefix from active RIB table in browser DOM', async ({ page, request }) => {
  200 |     // Trigger route withdrawal on frr2 via control server
  201 |     const withRes = await request.post(`${CTRL_URL}/frr/withdraw`);
  202 |     expect(withRes.status()).toBe(200);
  203 | 
  204 |     await page.goto(`${BONSAI_URL}/#/routing`);
  205 |     await page.waitForLoadState('domcontentloaded');
  206 | 
  207 |     const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
  208 |     const ribPanel = page.locator('.panel', { hasText: 'Canonical RIB Explorer' });
  209 | 
  210 |     // Poll Filter button until 10.10.1.0/24 is removed from table
  211 |     await expect(async () => {
  212 |       await filterBtn.click();
  213 |       const text = await ribPanel.textContent();
  214 |       expect(text).not.toContain('10.10.1.0/24');
  215 |     }).toPass({ timeout: 20_000, intervals: [1000] });
  216 | 
  217 |     await page.screenshot({ path: path.join(EVIDENCE_DIR, '03_route_withdrawn.png'), fullPage: true });
  218 |   });
  219 | 
  220 |   test('7. Re-announcement and router-driven TCP EOF disconnect preserves routes and updates completeness to gap', async ({ page, request }) => {
  221 |     // Re-announce 10.10.1.0/24
  222 |     const reannRes = await request.post(`${CTRL_URL}/frr/announce`);
  223 |     expect(reannRes.status()).toBe(200);
  224 | 
  225 |     // Wait until route reappears
  226 |     await page.goto(`${BONSAI_URL}/#/routing`);
  227 |     const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
  228 |     const ribTable = page.locator('.panel', { hasText: 'Canonical RIB Explorer' }).locator('.data-table');
  229 | 
  230 |     await expect(async () => {
  231 |       await filterBtn.click();
  232 |       await expect(ribTable).toContainText('10.10.1.0/24', { timeout: 2000 });
  233 |     }).toPass({ timeout: 20_000, intervals: [1000] });
  234 | 
  235 |     // Trigger router-driven BMP TCP EOF disconnect while BGP session stays Up
  236 |     const discRes = await request.post(`${CTRL_URL}/bmp/disconnect`);
  237 |     expect(discRes.status()).toBe(200);
  238 | 
  239 |     // Poll Refresh until Header badge changes to gap
  240 |     const refreshBtn = page.locator('button.btn-refresh');
  241 |     const compBadge = page.locator('.workspace-header .badge', { hasText: /Completeness: gap/i });
  242 |     await expect(async () => {
  243 |       await refreshBtn.click();
  244 |       await expect(compBadge).toBeVisible({ timeout: 1000 });
  245 |     }).toPass({ timeout: 15_000, intervals: [1000] });
  246 | 
  247 |     // Data-quality notice alert for transport gap is displayed
  248 |     const gapAlert = page.locator('.alert.alert-warn', { hasText: /transport disconnect/i });
  249 |     await expect(gapAlert).toBeVisible();
  250 | 
  251 |     // CRUCIAL: Routes are PRESERVED in table with completeness badge 'gap' (no synthesized withdrawals)
  252 |     await expect(ribTable).toContainText('10.10.1.0/24');
  253 |     const rowGapBadge = ribTable.locator('.badge', { hasText: 'gap' });
  254 |     await expect(rowGapBadge.first()).toBeVisible();
  255 | 
  256 |     // Verify route count did NOT drop to 0 on TCP EOF
  257 |     const routesKpi = page.locator('.kpi-card', { hasText: 'Active RIB Routes' }).locator('.kpi-value');
  258 |     await expect(routesKpi).not.toHaveText('0');
  259 | 
  260 |     await page.screenshot({ path: path.join(EVIDENCE_DIR, '04_transport_gap_preservation.png'), fullPage: true });
  261 |   });
  262 | 
  263 |   test('8. Reconnect on session epoch 2 resolves transport gap and restores complete state', async ({ page, request }) => {
  264 |     // Enable and accept epoch 2 reconnect
  265 |     const reconRes = await request.post(`${CTRL_URL}/bmp/reconnect`);
  266 |     expect(reconRes.status()).toBe(200);
  267 | 
  268 |     await page.goto(`${BONSAI_URL}/#/routing`);
  269 |     const refreshBtn = page.locator('button.btn-refresh');
  270 | 
  271 |     // Poll until completeness updates to complete
  272 |     await expect(async () => {
  273 |       await refreshBtn.click();
  274 |       const badgeText = await page.locator('.workspace-header .badge', { hasText: /Completeness:/i }).textContent();
  275 |       expect(badgeText).toContain('complete');
  276 |       expect(badgeText).not.toContain('gap');
> 277 |     }).toPass({ timeout: 20_000, intervals: [1000] });
      |        ^ Error: expect(received).toContain(expected) // indexOf
  278 | 
  279 |     // Verify exporters table remains populated
  280 |     const exportersTable = page.locator('.panel', { hasText: 'BMP Exporters' }).locator('.data-table');
  281 |     await expect(exportersTable).toContainText('172.100.207.11');
  282 | 
  283 |     await page.screenshot({ path: path.join(EVIDENCE_DIR, '05_reconnect_epoch2.png'), fullPage: true });
  284 |   });
  285 | 
  286 |   test('9. Prefix History Timeline renders announcement, modification and withdrawal events with exact attributes', async ({ page }) => {
  287 |     await page.goto(`${BONSAI_URL}/#/routing`);
  288 |     await page.waitForLoadState('domcontentloaded');
  289 | 
  290 |     // Load Timeline for 10.10.1.0/24
  291 |     const historyInput = page.locator('input[aria-label="Prefix History Query"]');
  292 |     await historyInput.fill('10.10.1.0/24');
  293 |     const loadBtn = page.locator('button', { hasText: 'Load Timeline' });
  294 |     await loadBtn.click();
  295 | 
  296 |     const timelineTable = page.locator('.panel', { hasText: 'Prefix Event History Timeline' }).locator('.data-table');
  297 |     await expect(timelineTable).toBeVisible({ timeout: 10_000 });
  298 | 
  299 |     // Verify event badges: Announce and Withdraw
  300 |     await expect(timelineTable.locator('.badge', { hasText: 'Announce' }).first()).toBeVisible();
  301 |     await expect(timelineTable.locator('.badge', { hasText: 'Withdraw' }).first()).toBeVisible();
  302 | 
  303 |     // Verify before/after Path Attributes columns (NH, AS path before and prepend, MED)
  304 |     await expect(timelineTable).toContainText('NH: 10.100.1.1');
  305 |     await expect(timelineTable).toContainText('AS: 65002');
  306 |     await expect(timelineTable).toContainText('65002 65002');
  307 |     await expect(timelineTable).toContainText('MED: 150');
  308 | 
  309 |     await page.screenshot({ path: path.join(EVIDENCE_DIR, '06_prefix_history_timeline.png'), fullPage: true });
  310 |   });
  311 | 
  312 |   test('10. Truthful empty filter and unsupported family states are displayed accurately', async ({ page }) => {
  313 |     await page.goto(`${BONSAI_URL}/#/routing`);
  314 |     await page.waitForLoadState('domcontentloaded');
  315 | 
  316 |     // 1. Truthful empty state
  317 |     const pfxFilter = page.locator('input[aria-label="Prefix Filter"]');
  318 |     await pfxFilter.fill('198.51.100.99/32');
  319 |     const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
  320 |     await filterBtn.click();
  321 | 
  322 |     const ribPanel = page.locator('.panel', { hasText: 'Canonical RIB Explorer' });
  323 |     await expect(ribPanel).toContainText(/No routes match current filter/i, { timeout: 5000 });
  324 | 
  325 |     // 2. Truthful unsupported state
  326 |     await pfxFilter.fill('');
  327 |     const famSelect = page.locator('select[aria-label="Address Family"]');
  328 |     await famSelect.selectOption('bgp-ls');
  329 |     await filterBtn.click();
  330 | 
  331 |     await expect(page.locator('.alert.alert-warn')).toContainText(/unsupported/i);
  332 |     await expect(ribPanel).toContainText(/Address family '?bgp-ls'? has (support )?status/i);
  333 | 
  334 |     await page.screenshot({ path: path.join(EVIDENCE_DIR, '07_empty_and_unsupported_states.png'), fullPage: true });
  335 |   });
  336 | 
  337 |   test('11. Routing Service restart from durable journals preserves complete state', async ({ page, request }) => {
  338 |     // Reset address family filter back to ipv4-unicast
  339 |     await page.goto(`${BONSAI_URL}/#/routing`);
  340 |     const famSelect = page.locator('select[aria-label="Address Family"]');
  341 |     await famSelect.selectOption('ipv4-unicast');
  342 |     const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
  343 |     await filterBtn.click();
  344 | 
  345 |     // Request restart of routing service from durable journals
  346 |     const restartRes = await request.post(`${CTRL_URL}/routing/restart`);
  347 |     expect(restartRes.status()).toBe(200);
  348 | 
  349 |     // Refresh UI and verify routes and exporters preserved from durable journals
  350 |     const refreshBtn = page.locator('button.btn-refresh');
  351 |     const ribTable = page.locator('.panel', { hasText: 'Canonical RIB Explorer' }).locator('.data-table');
  352 |     const exportersTable = page.locator('.panel', { hasText: 'BMP Exporters' }).locator('.data-table');
  353 | 
  354 |     await expect(async () => {
  355 |       await refreshBtn.click();
  356 |       await expect(ribTable).toContainText('10.10.1.0/24', { timeout: 2000 });
  357 |       await expect(ribTable).toContainText('10.99.1.0/24', { timeout: 2000 });
  358 |     }).toPass({ timeout: 20_000, intervals: [1000] });
  359 | 
  360 |     await expect(exportersTable).toContainText('172.100.207.11');
  361 |     await expect(exportersTable).toContainText('172.100.207.12');
  362 | 
  363 |     await page.screenshot({ path: path.join(EVIDENCE_DIR, '08_after_restart_preserved.png'), fullPage: true });
  364 |   });
  365 | 
  366 |   test('12. Logout revokes browser session and subsequent navigation is denied', async ({ page, request }) => {
  367 |     // Capture the pre-logout token before logout
  368 |     const cookiesBefore = await page.context().cookies(BONSAI_URL);
  369 |     const preLogoutToken = cookiesBefore.find(c => c.name === 'bonsai_token')?.value;
  370 |     expect(preLogoutToken).toBeTruthy();
  371 | 
  372 |     // Perform logout via page.request
  373 |     const logoutRes = await page.request.post(`${BONSAI_URL}/api/auth/logout`);
  374 |     expect(logoutRes.status()).toBe(204);
  375 | 
  376 |     // Verify Set-Cookie header cleared the token
  377 |     const cookiesAfter = await page.context().cookies(BONSAI_URL);
```