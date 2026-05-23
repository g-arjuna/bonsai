<script>
  import { onMount } from 'svelte';
  import { toast } from '$lib/toast.svelte.js';

  // ── State ───────────────────────────────────────────────────────────────────
  let loading = $state(true);
  let securityPosture = $state([]);
  let securityIncidents = $state([]);
  let vulnerabilities = $state([]);
  let securityPolicies = $state([]);
  let selectedTab = $state('settings');
  
  // Security configuration state
  let securityConfig = $state({
    database: { enabled: false },
    mfa: { enabled: false },
    session: { enabled: false },
    threat_intel: { enabled: false },
    incident_response: { enabled: false },
    anomaly_detection: { enabled: false }
  });
  let configLoading = $state(false);
  let configSaving = $state(false);

  // ── Data Loading ─────────────────────────────────────────────────────────────
  async function loadSecurityPosture() {
    try {
      const r = await fetch('/api/graph/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          query: `MATCH (d:Device)-[:HAS_POSTURE]->(sp:SecurityPosture)
                  RETURN d.address as device, d.hostname as hostname, 
                         sp.risk_score as risk_score, sp.aaa_failure_count as aaa_failures,
                         sp.config_change_count as config_changes, sp.process_crash_count as process_crashes,
                         sp.updated_at_ns as last_updated
                  ORDER BY sp.risk_score DESC`
        })
      });
      if (r.ok) {
        const data = await r.json();
        securityPosture = data.results || [];
      }
    } catch (error) {
      toast.error('Failed to load security posture');
    }
  }

  async function loadSecurityIncidents() {
    try {
      const r = await fetch('/api/graph/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          query: `MATCH (d:Device)-[:HAS_SECURITY_INCIDENT]->(si:SecurityIncident)
                  RETURN d.address as device, d.hostname as hostname,
                         si.incident_type as type, si.severity as severity,
                         si.title as title, si.status as status,
                         si.detected_at_ns as detected_at, si.assigned_to as assigned_to
                  ORDER BY si.detected_at_ns DESC`
        })
      });
      if (r.ok) {
        const data = await r.json();
        securityIncidents = data.results || [];
      }
    } catch (error) {
      toast.error('Failed to load security incidents');
    }
  }

  async function loadVulnerabilities() {
    try {
      const r = await fetch('/api/graph/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          query: `MATCH (d:Device)-[:VULNERABLE_TO]->(v:Vulnerability)
                  RETURN d.address as device, d.hostname as hostname,
                         v.cve_id as cve_id, v.severity as severity,
                         v.cvss_score as cvss_score, v.title as title,
                         v.patched_at_ns as patched_at
                  ORDER BY v.cvss_score DESC`
        })
      });
      if (r.ok) {
        const data = await r.json();
        vulnerabilities = data.results || [];
      }
    } catch (error) {
      toast.error('Failed to load vulnerabilities');
    }
  }

  async function loadSecurityPolicies() {
    try {
      const r = await fetch('/api/graph/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          query: `MATCH (d:Device)-[:ENFORCES_POLICY]->(sp:SecurityPolicy)
                  RETURN d.address as device, d.hostname as hostname,
                         sp.policy_name as policy_name, sp.policy_type as policy_type,
                         sp.compliance_framework as framework, sp.enforcement_status as status,
                         sp.compliance_status as compliance_status
                  ORDER BY sp.policy_name`
        })
      });
      if (r.ok) {
        const data = await r.json();
        securityPolicies = data.results || [];
      }
    } catch (error) {
      toast.error('Failed to load security policies');
    }
  }

  // ── Security Configuration Functions ─────────────────────────────────────
  async function loadSecurityConfig() {
    try {
      configLoading = true;
      const r = await fetch('/api/settings/security');
      if (r.ok) {
        const config = await r.json();
        securityConfig = config;
      } else {
        toast.error('Failed to load security configuration');
      }
    } catch (error) {
      toast.error('Failed to load security configuration');
    } finally {
      configLoading = false;
    }
  }

  async function saveSecurityConfig() {
    try {
      configSaving = true;
      const r = await fetch('/api/settings/security', {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(securityConfig)
      });
      if (r.ok) {
        toast.success('Security configuration saved successfully');
      } else {
        const error = await r.text();
        toast.error(`Failed to save security configuration: ${error}`);
      }
    } catch (error) {
      toast.error('Failed to save security configuration');
    } finally {
      configSaving = false;
    }
  }

  async function toggleSecurityModule(module, enabled) {
    securityConfig[module].enabled = enabled;
    await saveSecurityConfig();
  }

  async function enableAllSecurity() {
    for (const module in securityConfig) {
      securityConfig[module].enabled = true;
    }
    await saveSecurityConfig();
  }

  async function disableAllSecurity() {
    for (const module in securityConfig) {
      securityConfig[module].enabled = false;
    }
    await saveSecurityConfig();
  }

  function formatTimestamp(ns) {
    return new Date(ns / 1_000_000).toLocaleString();
  }

  function getRiskColor(score) {
    if (score >= 8.0) return 'red';
    if (score >= 6.0) return 'orange';
    if (score >= 4.0) return 'yellow';
    return 'green';
  }

  function getSeverityColor(severity) {
    switch (severity?.toLowerCase()) {
      case 'critical': return 'red';
      case 'high': return 'orange';
      case 'medium': return 'yellow';
      case 'low': return 'green';
      default: return 'gray';
    }
  }

  onMount(async () => {
    await Promise.all([
      loadSecurityConfig(),
      loadSecurityPosture(),
      loadSecurityIncidents(),
      loadVulnerabilities(),
      loadSecurityPolicies()
    ]);
    loading = false;
  });
</script>

<div class="security-dashboard">
  <div class="header">
    <h1>🔒 Security Command Center</h1>
    <p>Real-time security posture, incidents, and vulnerability management</p>
  </div>

  {#if loading}
    <div class="loading">Loading security data...</div>
  {:else}
    <!-- Tab Navigation -->
    <div class="tabs">
      <button 
        class={selectedTab === 'settings' ? 'active' : ''}
        onclick={() => selectedTab = 'settings'}
      >
        ⚙️ Security Settings
      </button>
      <button 
        class={selectedTab === 'posture' ? 'active' : ''}
        onclick={() => selectedTab = 'posture'}
      >
        📊 Security Posture
      </button>
      <button 
        class={selectedTab === 'incidents' ? 'active' : ''}
        onclick={() => selectedTab = 'incidents'}
      >
        🚨 Security Incidents
      </button>
      <button 
        class={selectedTab === 'vulnerabilities' ? 'active' : ''}
        onclick={() => selectedTab = 'vulnerabilities'}
      >
        🔓 Vulnerabilities
      </button>
      <button 
        class={selectedTab === 'policies' ? 'active' : ''}
        onclick={() => selectedTab = 'policies'}
      >
        📋 Security Policies
      </button>
    </div>

    <!-- Security Settings Tab -->
    {#if selectedTab === 'settings'}
      <div class="tab-content">
        <div class="settings-header">
          <h2>⚙️ Advanced Security Settings</h2>
          <p>Enable or disable security features. All features are disabled by default for testing environments.</p>
          <div class="bulk-actions">
            <button 
              class="btn btn-success" 
              onclick={enableAllSecurity}
              disabled={configSaving}
            >
              {configSaving ? '...' : '✅ Enable All'}
            </button>
            <button 
              class="btn btn-danger" 
              onclick={disableAllSecurity}
              disabled={configSaving}
            >
              {configSaving ? '...' : '❌ Disable All'}
            </button>
          </div>
        </div>

        {#if configLoading}
          <div class="loading">Loading security configuration...</div>
        {:else}
          <div class="security-modules">
            <!-- Database Security -->
            <div class="security-module">
              <div class="module-header">
                <div class="module-info">
                  <h3>🗄️ Database Security</h3>
                  <p>Encryption, auditing, access controls, and data masking</p>
                </div>
                <label class="toggle-switch">
                  <input 
                    type="checkbox" 
                    checked={securityConfig.database.enabled}
                    onchange={(e) => toggleSecurityModule('database', e.target.checked)}
                    disabled={configSaving}
                  />
                  <span class="slider"></span>
                </label>
              </div>
              <div class="module-details">
                <div class="feature-list">
                  <div class="feature-item">
                    <span class="feature-name">🔐 Data Encryption</span>
                    <span class="feature-status" class:enabled={securityConfig.database.enabled}>{
                      securityConfig.database.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">📊 Security Auditing</span>
                    <span class="feature-status" class:enabled={securityConfig.database.enabled}>{
                      securityConfig.database.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🔑 Access Controls</span>
                    <span class="feature-status" class:enabled={securityConfig.database.enabled}>{
                      securityConfig.database.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🎭 Data Masking</span>
                    <span class="feature-status" class:enabled={securityConfig.database.enabled}>{
                      securityConfig.database.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                </div>
              </div>
            </div>

            <!-- MFA -->
            <div class="security-module">
              <div class="module-header">
                <div class="module-info">
                  <h3>🔐 Multi-Factor Authentication</h3>
                  <p>TOTP, SMS, Email, and backup codes</p>
                </div>
                <label class="toggle-switch">
                  <input 
                    type="checkbox" 
                    checked={securityConfig.mfa.enabled}
                    onchange={(e) => toggleSecurityModule('mfa', e.target.checked)}
                    disabled={configSaving}
                  />
                  <span class="slider"></span>
                </label>
              </div>
              <div class="module-details">
                <div class="feature-list">
                  <div class="feature-item">
                    <span class="feature-name">📱 TOTP (Time-based OTP)</span>
                    <span class="feature-status" class:enabled={securityConfig.mfa.enabled}>{
                      securityConfig.mfa.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">💬 SMS Authentication</span>
                    <span class="feature-status" class:enabled={securityConfig.mfa.enabled}>{
                      securityConfig.mfa.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">📧 Email Authentication</span>
                    <span class="feature-status" class:enabled={securityConfig.mfa.enabled}>{
                      securityConfig.mfa.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🔢 Backup Codes</span>
                    <span class="feature-status" class:enabled={securityConfig.mfa.enabled}>{
                      securityConfig.mfa.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                </div>
              </div>
            </div>

            <!-- Session Management -->
            <div class="security-module">
              <div class="module-header">
                <div class="module-info">
                  <h3>🎫 Session Management</h3>
                  <p>JWT tokens, revocation, and automatic cleanup</p>
                </div>
                <label class="toggle-switch">
                  <input 
                    type="checkbox" 
                    checked={securityConfig.session.enabled}
                    onchange={(e) => toggleSecurityModule('session', e.target.checked)}
                    disabled={configSaving}
                  />
                  <span class="slider"></span>
                </label>
              </div>
              <div class="module-details">
                <div class="feature-list">
                  <div class="feature-item">
                    <span class="feature-name">🔑 JWT Token Management</span>
                    <span class="feature-status" class:enabled={securityConfig.session.enabled}>{
                      securityConfig.session.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🚫 Token Revocation</span>
                    <span class="feature-status" class:enabled={securityConfig.session.enabled}>{
                      securityConfig.session.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">⏰ Session Timeout</span>
                    <span class="feature-status" class:enabled={securityConfig.session.enabled}>{
                      securityConfig.session.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🧹 Auto Cleanup</span>
                    <span class="feature-status" class:enabled={securityConfig.session.enabled}>{
                      securityConfig.session.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                </div>
              </div>
            </div>

            <!-- Threat Intelligence -->
            <div class="security-module">
              <div class="module-header">
                <div class="module-info">
                  <h3>🛡️ Threat Intelligence</h3>
                  <p>Real-time threat feeds and automated blocking</p>
                </div>
                <label class="toggle-switch">
                  <input 
                    type="checkbox" 
                    checked={securityConfig.threat_intel.enabled}
                    onchange={(e) => toggleSecurityModule('threat_intel', e.target.checked)}
                    disabled={configSaving}
                  />
                  <span class="slider"></span>
                </label>
              </div>
              <div class="module-details">
                <div class="feature-list">
                  <div class="feature-item">
                    <span class="feature-name">📡 Real-time Threat Feeds</span>
                    <span class="feature-status" class:enabled={securityConfig.threat_intel.enabled}>{
                      securityConfig.threat_intel.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🎯 Indicator Caching</span>
                    <span class="feature-status" class:enabled={securityConfig.threat_intel.enabled}>{
                      securityConfig.threat_intel.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🚫 Automated IP Blocking</span>
                    <span class="feature-status" class:enabled={securityConfig.threat_intel.enabled}>{
                      securityConfig.threat_intel.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">📈 Threat Analytics</span>
                    <span class="feature-status" class:enabled={securityConfig.threat_intel.enabled}>{
                      securityConfig.threat_intel.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                </div>
              </div>
            </div>

            <!-- Incident Response -->
            <div class="security-module">
              <div class="module-header">
                <div class="module-info">
                  <h3>🚨 Incident Response</h3>
                  <p>Automated workflows and notification systems</p>
                </div>
                <label class="toggle-switch">
                  <input 
                    type="checkbox" 
                    checked={securityConfig.incident_response.enabled}
                    onchange={(e) => toggleSecurityModule('incident_response', e.target.checked)}
                    disabled={configSaving}
                  />
                  <span class="slider"></span>
                </label>
              </div>
              <div class="module-details">
                <div class="feature-list">
                  <div class="feature-item">
                    <span class="feature-name">⚡ Automated Workflows</span>
                    <span class="feature-status" class:enabled={securityConfig.incident_response.enabled}>{
                      securityConfig.incident_response.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">✅ Approval Chains</span>
                    <span class="feature-status" class:enabled={securityConfig.incident_response.enabled}>{
                      securityConfig.incident_response.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">📢 Notification Systems</span>
                    <span class="feature-status" class:enabled={securityConfig.incident_response.enabled}>{
                      securityConfig.incident_response.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">📊 Incident Analytics</span>
                    <span class="feature-status" class:enabled={securityConfig.incident_response.enabled}>{
                      securityConfig.incident_response.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                </div>
              </div>
            </div>

            <!-- Anomaly Detection -->
            <div class="security-module">
              <div class="module-header">
                <div class="module-info">
                  <h3>🔍 Anomaly Detection</h3>
                  <p>Statistical analysis and pattern detection</p>
                </div>
                <label class="toggle-switch">
                  <input 
                    type="checkbox" 
                    checked={securityConfig.anomaly_detection.enabled}
                    onchange={(e) => toggleSecurityModule('anomaly_detection', e.target.checked)}
                    disabled={configSaving}
                  />
                  <span class="slider"></span>
                </label>
              </div>
              <div class="module-details">
                <div class="feature-list">
                  <div class="feature-item">
                    <span class="feature-name">📊 Statistical Analysis</span>
                    <span class="feature-status" class:enabled={securityConfig.anomaly_detection.enabled}>{
                      securityConfig.anomaly_detection.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🎯 Pattern Detection</span>
                    <span class="feature-status" class:enabled={securityConfig.anomaly_detection.enabled}>{
                      securityConfig.anomaly_detection.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">👤 User Behavior Analysis</span>
                    <span class="feature-status" class:enabled={securityConfig.anomaly_detection.enabled}>{
                      securityConfig.anomaly_detection.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                  <div class="feature-item">
                    <span class="feature-name">🤖 ML-based Detection</span>
                    <span class="feature-status" class:enabled={securityConfig.anomaly_detection.enabled}>{
                      securityConfig.anomaly_detection.enabled ? 'Available' : 'Disabled'
                    }</span>
                  </div>
                </div>
              </div>
            </div>
          </div>
        {/if}
      </div>
    <!-- Security Posture Tab -->
    {#if selectedTab === 'posture'}
      <div class="tab-content">
        <h2>📊 Security Posture Overview</h2>
        <div class="posture-grid">
          {#each securityPosture as posture}
            <div class="posture-card" style="border-left: 4px solid {getRiskColor(posture.risk_score)}">
              <div class="posture-header">
                <h3>{posture.hostname || posture.device}</h3>
                <span class="risk-score" style="color: {getRiskColor(posture.risk_score)}">
                  Risk: {posture.risk_score?.toFixed(1) || '0.0'}
                </span>
              </div>
              <div class="posture-metrics">
                <div class="metric">
                  <span class="label">AAA Failures:</span>
                  <span class="value">{posture.aaa_failures || 0}</span>
                </div>
                <div class="metric">
                  <span class="label">Config Changes:</span>
                  <span class="value">{posture.config_changes || 0}</span>
                </div>
                <div class="metric">
                  <span class="label">Process Crashes:</span>
                  <span class="value">{posture.process_crashes || 0}</span>
                </div>
              </div>
              <div class="posture-footer">
                <small>Last updated: {formatTimestamp(posture.last_updated)}</small>
              </div>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    <!-- Security Incidents Tab -->
    {#if selectedTab === 'incidents'}
      <div class="tab-content">
        <h2>🚨 Security Incidents</h2>
        <div class="incidents-table">
          <table>
            <thead>
              <tr>
                <th>Device</th>
                <th>Type</th>
                <th>Title</th>
                <th>Severity</th>
                <th>Status</th>
                <th>Detected</th>
                <th>Assigned To</th>
              </tr>
            </thead>
            <tbody>
              {#each securityIncidents as incident}
                <tr>
                  <td>{incident.hostname || incident.device}</td>
                  <td>{incident.type}</td>
                  <td>{incident.title}</td>
                  <td>
                    <span class="severity-badge" style="background-color: {getSeverityColor(incident.severity)}">
                      {incident.severity}
                    </span>
                  </td>
                  <td>{incident.status}</td>
                  <td>{formatTimestamp(incident.detected_at)}</td>
                  <td>{incident.assigned_to || 'Unassigned'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </div>
    {/if}

    <!-- Vulnerabilities Tab -->
    {#if selectedTab === 'vulnerabilities'}
      <div class="tab-content">
        <h2>🔓 Vulnerability Management</h2>
        <div class="vulnerabilities-grid">
          {#each vulnerabilities as vuln}
            <div class="vulnerability-card" style="border-left: 4px solid {getSeverityColor(vuln.severity)}">
              <div class="vulnerability-header">
                <h3>{vuln.cve_id}</h3>
                <span class="cvss-score" style="color: {getSeverityColor(vuln.severity)}">
                  CVSS: {vuln.cvss_score?.toFixed(1) || '0.0'}
                </span>
              </div>
              <div class="vulnerability-title">{vuln.title}</div>
              <div class="vulnerability-device">
                <strong>Device:</strong> {vuln.hostname || vuln.device}
              </div>
              <div class="vulnerability-footer">
                {#if vuln.patched_at}
                  <span class="patched">✅ Patched: {formatTimestamp(vuln.patched_at)}</span>
                {:else}
                  <span class="unpatched">⚠️ Unpatched</span>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    <!-- Security Policies Tab -->
    {#if selectedTab === 'policies'}
      <div class="tab-content">
        <h2>📋 Security Policies</h2>
        <div class="policies-table">
          <table>
            <thead>
              <tr>
                <th>Device</th>
                <th>Policy Name</th>
                <th>Type</th>
                <th>Framework</th>
                <th>Enforcement Status</th>
                <th>Compliance Status</th>
              </tr>
            </thead>
            <tbody>
              {#each securityPolicies as policy}
                <tr>
                  <td>{policy.hostname || policy.device}</td>
                  <td>{policy.policy_name}</td>
                  <td>{policy.policy_type}</td>
                  <td>{policy.framework}</td>
                  <td>{policy.status}</td>
                  <td>
                    <span class="compliance-badge" class:compliant={policy.compliance_status === 'compliant'}>
                      {policy.compliance_status}
                    </span>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </div>
    {/if}
  {/if}
</div>

<style>
  .security-dashboard {
    padding: 2rem;
    max-width: 1400px;
    margin: 0 auto;
  }

  .header {
    text-align: center;
    margin-bottom: 2rem;
  }

  .header h1 {
    color: #2563eb;
    margin-bottom: 0.5rem;
  }

  .header p {
    color: #6b7280;
    font-size: 1.1rem;
  }

  .loading {
    text-align: center;
    padding: 4rem;
    color: #6b7280;
  }

  .tabs {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 2rem;
    border-bottom: 2px solid #e5e7eb;
  }

  .tabs button {
    padding: 0.75rem 1.5rem;
    border: none;
    background: none;
    color: #6b7280;
    cursor: pointer;
    border-bottom: 2px solid transparent;
    transition: all 0.2s;
  }

  .tabs button.active {
    color: #2563eb;
    border-bottom-color: #2563eb;
  }

  .tabs button:hover {
    color: #2563eb;
  }

  .tab-content {
    margin-top: 2rem;
  }

  .tab-content h2 {
    margin-bottom: 1.5rem;
    color: #1f2937;
  }

  .posture-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(350px, 1fr));
    gap: 1.5rem;
  }

  .posture-card {
    background: white;
    border-radius: 8px;
    padding: 1.5rem;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
  }

  .posture-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
  }

  .posture-header h3 {
    margin: 0;
    color: #1f2937;
  }

  .risk-score {
    font-weight: bold;
    font-size: 1.1rem;
  }

  .posture-metrics {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }

  .metric {
    display: flex;
    justify-content: space-between;
  }

  .metric .label {
    color: #6b7280;
  }

  .metric .value {
    font-weight: bold;
    color: #1f2937;
  }

  .posture-footer {
    color: #6b7280;
    font-size: 0.875rem;
  }

  .incidents-table, .policies-table {
    background: white;
    border-radius: 8px;
    overflow: hidden;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
  }

  .incidents-table table, .policies-table table {
    width: 100%;
    border-collapse: collapse;
  }

  .incidents-table th, .policies-table th {
    background: #f9fafb;
    padding: 1rem;
    text-align: left;
    font-weight: 600;
    color: #1f2937;
    border-bottom: 1px solid #e5e7eb;
  }

  .incidents-table td, .policies-table td {
    padding: 1rem;
    border-bottom: 1px solid #e5e7eb;
  }

  .severity-badge {
    color: white;
    padding: 0.25rem 0.75rem;
    border-radius: 12px;
    font-size: 0.875rem;
    font-weight: 500;
  }

  .compliance-badge {
    padding: 0.25rem 0.75rem;
    border-radius: 12px;
    font-size: 0.875rem;
    font-weight: 500;
    background-color: #fca5a5;
    color: #7f1d1d;
  }

  .compliance-badge.compliant {
    background-color: #86efac;
    color: #14532d;
  }

  .vulnerabilities-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(400px, 1fr));
    gap: 1.5rem;
  }

  .vulnerability-card {
    background: white;
    border-radius: 8px;
    padding: 1.5rem;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
  }

  .vulnerability-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
  }

  .vulnerability-header h3 {
    margin: 0;
    color: #1f2937;
  }

  .cvss-score {
    font-weight: bold;
    font-size: 1.1rem;
  }

  .vulnerability-title {
    color: #4b5563;
    margin-bottom: 0.5rem;
  }

  .vulnerability-device {
    color: #6b7280;
    margin-bottom: 1rem;
  }

  .vulnerability-footer {
    font-size: 0.875rem;
  }

  .patched {
    color: #059669;
    font-weight: 500;
  }

  .unpatched {
    color: #dc2626;
    font-weight: 500;
  }

  /* Security Settings Styles */
  .settings-header {
    margin-bottom: 2rem;
  }

  .settings-header h2 {
    margin-bottom: 0.5rem;
    color: #1f2937;
  }

  .settings-header p {
    color: #6b7280;
    margin-bottom: 1rem;
  }

  .bulk-actions {
    display: flex;
    gap: 1rem;
  }

  .btn {
    padding: 0.5rem 1rem;
    border: none;
    border-radius: 6px;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
  }

  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn-success {
    background-color: #059669;
    color: white;
  }

  .btn-success:hover:not(:disabled) {
    background-color: #047857;
  }

  .btn-danger {
    background-color: #dc2626;
    color: white;
  }

  .btn-danger:hover:not(:disabled) {
    background-color: #b91c1c;
  }

  .security-modules {
    display: grid;
    gap: 1.5rem;
  }

  .security-module {
    background: white;
    border-radius: 12px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
    overflow: hidden;
    border: 1px solid #e5e7eb;
  }

  .module-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 1.5rem;
    background: #f9fafb;
    border-bottom: 1px solid #e5e7eb;
  }

  .module-info h3 {
    margin: 0 0 0.5rem 0;
    color: #1f2937;
    font-size: 1.25rem;
  }

  .module-info p {
    margin: 0;
    color: #6b7280;
    font-size: 0.875rem;
  }

  /* Toggle Switch */
  .toggle-switch {
    position: relative;
    display: inline-block;
    width: 60px;
    height: 34px;
  }

  .toggle-switch input {
    opacity: 0;
    width: 0;
    height: 0;
  }

  .slider {
    position: absolute;
    cursor: pointer;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background-color: #ccc;
    transition: .4s;
    border-radius: 34px;
  }

  .slider:before {
    position: absolute;
    content: "";
    height: 26px;
    width: 26px;
    left: 4px;
    bottom: 4px;
    background-color: white;
    transition: .4s;
    border-radius: 50%;
  }

  input:checked + .slider {
    background-color: #059669;
  }

  input:checked + .slider:before {
    transform: translateX(26px);
  }

  input:disabled + .slider {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .module-details {
    padding: 1.5rem;
  }

  .feature-list {
    display: grid;
    gap: 0.75rem;
  }

  .feature-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.75rem;
    background: #f9fafb;
    border-radius: 6px;
    border: 1px solid #e5e7eb;
  }

  .feature-name {
    font-weight: 500;
    color: #374151;
  }

  .feature-status {
    padding: 0.25rem 0.75rem;
    border-radius: 12px;
    font-size: 0.75rem;
    font-weight: 500;
    background-color: #fca5a5;
    color: #7f1d1d;
  }

  .feature-status.enabled {
    background-color: #86efac;
    color: #14532d;
  }
</style>
