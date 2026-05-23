// Service Discovery Module for Bonsai
// Implements heuristics for detecting service endpoints from telemetry

use std::collections::HashMap;
use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use regex::Regex;

use super::graph::common::{read_str, now_ns};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    pub id: String,
    pub device_address: String,
    pub interface_name: String,
    pub service_type: String,
    pub service_name: String,
    pub endpoint_type: String,
    pub connection_count: i64,
    pub avg_throughput_mbps: f64,
    pub discovered_via: String,
    pub confidence_score: f64,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDiscoveryConfig {
    pub interface_patterns: HashMap<String, f64>,
    pub traffic_thresholds: TrafficThresholds,
    pub service_ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficThresholds {
    pub high_connection_threshold: i64,
    pub periodic_traffic_threshold: f64,
    pub confidence_threshold: f64,
}

lazy_static::lazy_static! {
    static ref SERVICE_PATTERNS: Vec<(Regex, f64, String)> = vec![
        // API Gateway patterns
        (Regex::new(r"(?i)api[_-]?gateway").unwrap(), 0.9, "api_gateway"),
        (Regex::new(r"(?i)gateway[_-]?api").unwrap(), 0.9, "api_gateway"),
        (Regex::new(r"(?i)api[_-]?endpoint").unwrap(), 0.8, "api_gateway"),
        
        // Database patterns
        (Regex::new(r"(?i)database[_-]?server").unwrap(), 0.9, "database"),
        (Regex::new(r"(?i)db[_-]?server").unwrap(), 0.8, "database"),
        (Regex::new(r"(?i)mysql[_-]?server").unwrap(), 0.9, "database"),
        (Regex::new(r"(?i)postgres[_-]?server").unwrap(), 0.9, "database"),
        (Regex::new(r"(?i)mongodb[_-]?server").unwrap(), 0.9, "database"),
        
        // Cache patterns
        (Regex::new(r"(?i)cache[_-]?server").unwrap(), 0.9, "cache"),
        (Regex::new(r"(?i)redis[_-]?server").unwrap(), 0.9, "cache"),
        (Regex::new(r"(?i)memcached[_-]?server").unwrap(), 0.9, "cache"),
        
        // Load Balancer patterns
        (Regex::new(r"(?i)load[_-]?balancer").unwrap(), 0.9, "load_balancer"),
        (Regex::new(r"(?i)lb[_-]?frontend").unwrap(), 0.8, "load_balancer"),
        (Regex::new(r"(?i)haproxy[_-]?frontend").unwrap(), 0.9, "load_balancer"),
        
        // Web Server patterns
        (Regex::new(r"(?i)web[_-]?server").unwrap(), 0.7, "web_server"),
        (Regex::new(r"(?i)nginx[_-]?frontend").unwrap(), 0.8, "web_server"),
        (Regex::new(r"(?i)apache[_-]?frontend").unwrap(), 0.8, "web_server"),
        
        // Application Server patterns
        (Regex::new(r"(?i)application[_-]?server").unwrap(), 0.7, "application_server"),
        (Regex::new(r"(?i)app[_-]?server").unwrap(), 0.7, "application_server"),
        
        // Message Queue patterns
        (Regex::new(r"(?i)message[_-]?queue").unwrap(), 0.9, "message_queue"),
        (Regex::new(r"(?i)kafka[_-]?broker").unwrap(), 0.9, "message_queue"),
        (Regex::new(r"(?i)rabbitmq[_-]?server").unwrap(), 0.9, "message_queue"),
        
        // Service Mesh patterns
        (Regex::new(r"(?i)envoy[_-]?proxy").unwrap(), 0.9, "service_mesh"),
        (Regex::new(r"(?i)istio[_-]?proxy").unwrap(), 0.9, "service_mesh"),
        (Regex::new(r"(?i)consul[_-]?agent").unwrap(), 0.8, "service_mesh"),
        
        // Search Engine patterns
        (Regex::new(r"(?i)elasticsearch[_-]?node").unwrap(), 0.9, "search_engine"),
        (Regex::new(r"(?i)solr[_-]?server").unwrap(), 0.8, "search_engine"),
        
        // Monitoring patterns
        (Regex::new(r"(?i)prometheus[_-]?exporter").unwrap(), 0.8, "monitoring"),
        (Regex::new(r"(?i)grafana[_-]?server").unwrap(), 0.8, "monitoring"),
    ];
}

impl ServiceDiscoveryConfig {
    pub fn default() -> Self {
        let mut interface_patterns = HashMap::new();
        interface_patterns.insert("api_gateway".to_string(), 0.7);
        interface_patterns.insert("database".to_string(), 0.7);
        interface_patterns.insert("cache".to_string(), 0.7);
        interface_patterns.insert("load_balancer".to_string(), 0.7);
        interface_patterns.insert("web_server".to_string(), 0.7);
        interface_patterns.insert("application_server".to_string(), 0.7);
        interface_patterns.insert("message_queue".to_string(), 0.7);
        interface_patterns.insert("service_mesh".to_string(), 0.7);
        
        Self {
            interface_patterns,
            traffic_thresholds: TrafficThresholds {
                high_connection_threshold: 1000,
                periodic_traffic_threshold: 0.8,
                confidence_threshold: 0.6,
            },
            service_ports: vec![80, 443, 3306, 5432, 6379, 9092, 27017, 9200, 15672, 8500],
        }
    }
}

/// Service discovery engine
pub struct ServiceDiscovery {
    config: ServiceDiscoveryConfig,
}

impl ServiceDiscovery {
    pub fn new(config: ServiceDiscoveryConfig) -> Self {
        Self { config }
    }
    
    /// Discover service endpoints from interface descriptions
    pub fn discover_from_descriptions(
        &self,
        conn: &Connection<'_>,
        device_address: &str,
    ) -> Result<Vec<ServiceEndpoint>> {
        let mut endpoints = Vec::new();
        
        let rows = conn
            .query(
                "MATCH (d:Device {address: $device_address})-[:HAS_INTERFACE]->(i:Interface) \
                 WHERE i.description IS NOT NULL AND i.description <> '' \
                 RETURN i.name, i.description, i.speed",
                vec![("device_address", Value::String(device_address.to_string()))],
            )
            .context("query interface descriptions")?;
        
        for row in rows {
            let if_name = read_str(&row[0]);
            let description = read_str(&row[1]);
            let speed = read_i64(&row[2]);
            
            if let Some(endpoint) = self.analyze_interface_description(
                device_address,
                &if_name,
                &description,
                speed,
            ) {
                endpoints.push(endpoint);
            }
        }
        
        Ok(endpoints)
    }
    
    /// Discover service endpoints from traffic patterns
    pub fn discover_from_traffic_patterns(
        &self,
        conn: &Connection<'_>,
        device_address: &str,
    ) -> Result<Vec<ServiceEndpoint>> {
        let mut endpoints = Vec::new();
        
        // Look for interfaces with high connection counts or periodic traffic
        let rows = conn
            .query(
                "MATCH (d:Device {address: $device_address})-[:HAS_INTERFACE]->(i:Interface) \
                 WHERE i.in_octets > 0 OR i.out_octets > 0 \
                 RETURN i.name, i.in_octets, i.out_octets, i.speed, i.updated_at_ns \
                 ORDER BY i.updated_at_ns DESC LIMIT 1000",
                vec![("device_address", Value::String(device_address.to_string()))],
            )
            .context("query interface traffic patterns")?;
        
        let mut interface_traffic: HashMap<String, Vec<(f64, i64)>> = HashMap::new();
        
        for row in rows {
            let if_name = read_str(&row[0]);
            let in_octets = read_i64(&row[1]) as f64;
            let out_octets = read_i64(&row[2]) as f64;
            let speed = read_i64(&row[3]) as f64;
            let timestamp = read_i64(&row[4]);
            
            let throughput_mbps = ((in_octets + out_octets) * 8.0) / (speed * 1_000_000.0);
            interface_traffic.entry(if_name).or_insert_with(Vec::new).push((throughput_mbps, timestamp));
        }
        
        for (if_name, samples) in interface_traffic {
            if let Some(endpoint) = self.analyze_traffic_pattern(
                device_address,
                &if_name,
                &samples,
            ) {
                endpoints.push(endpoint);
            }
        }
        
        Ok(endpoints)
    }
    
    /// Analyze interface description for service indicators
    fn analyze_interface_description(
        &self,
        device_address: &str,
        if_name: &str,
        description: &str,
        speed: i64,
    ) -> Option<ServiceEndpoint> {
        for (pattern, confidence, service_type) in &SERVICE_PATTERNS {
            if pattern.is_match(description) {
                let service_name = self.extract_service_name(description, service_type);
                let endpoint_type = self.determine_endpoint_type(if_name, speed);
                
                return Some(ServiceEndpoint {
                    id: format!("service-{}-{}-{}", device_address, if_name, service_type),
                    device_address: device_address.to_string(),
                    interface_name: if_name.to_string(),
                    service_type: service_type.clone(),
                    service_name,
                    endpoint_type,
                    connection_count: 0, // Will be updated from traffic analysis
                    avg_throughput_mbps: 0.0, // Will be updated from traffic analysis
                    discovered_via: "interface_description".to_string(),
                    confidence_score: *confidence,
                    updated_at_ns: now_ns(),
                });
            }
        }
        None
    }
    
    /// Analyze traffic patterns for service indicators
    fn analyze_traffic_pattern(
        &self,
        device_address: &str,
        if_name: &str,
        samples: &[(f64, i64)],
    ) -> Option<ServiceEndpoint> {
        if samples.len() < 10 {
            return None;
        }
        
        // Calculate traffic statistics
        let avg_throughput: f64 = samples.iter().map(|(v, _)| *v).sum::<f64>() / samples.len() as f64;
        let max_throughput = samples.iter().map(|(v, _)| *v).fold(0.0, |a, b| a.max(b));
        
        // Check for periodic traffic patterns
        let periodic_score = self.calculate_periodic_score(samples);
        
        // Estimate connection count based on traffic patterns
        let estimated_connections = self.estimate_connection_count(avg_throughput, max_throughput);
        
        if estimated_connections > self.config.traffic_thresholds.high_connection_threshold
            || periodic_score > self.config.traffic_thresholds.periodic_traffic_threshold
        {
            let service_type = self.infer_service_type_from_traffic(avg_throughput, max_throughput);
            let confidence = self.calculate_traffic_confidence(
                estimated_connections,
                periodic_score,
                avg_throughput,
            );
            
            if confidence > self.config.traffic_thresholds.confidence_threshold {
                return Some(ServiceEndpoint {
                    id: format!("service-{}-{}-traffic", device_address, if_name),
                    device_address: device_address.to_string(),
                    interface_name: if_name.to_string(),
                    service_type,
                    service_name: format!("{}_service", if_name),
                    endpoint_type: "internal".to_string(),
                    connection_count: estimated_connections,
                    avg_throughput_mbps: avg_throughput,
                    discovered_via: "traffic_pattern".to_string(),
                    confidence_score: confidence,
                    updated_at_ns: now_ns(),
                });
            }
        }
        
        None
    }
    
    /// Extract service name from description
    fn extract_service_name(&self, description: &str, service_type: &str) -> String {
        // Try to extract a specific service name from the description
        let cleaned = description
            .trim()
            .to_lowercase()
            .replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "");
        
        if cleaned.is_empty() {
            format!("{}_service", service_type)
        } else {
            cleaned
        }
    }
    
    /// Determine endpoint type based on interface characteristics
    fn determine_endpoint_type(&self, if_name: &str, speed: i64) -> String {
        if if_name.contains("mgmt") || if_name.contains("admin") {
            "management".to_string()
        } else if speed >= 10_000_000_000 { // >= 10Gbps
            "northbound".to_string()
        } else if if_name.contains("loopback") || if_name.contains("lo") {
            "internal".to_string()
        } else {
            "southbound".to_string()
        }
    }
    
    /// Calculate periodic traffic pattern score
    fn calculate_periodic_score(&self, samples: &[(f64, i64)]) -> f64 {
        if samples.len() < 20 {
            return 0.0;
        }
        
        // Simple heuristic: check for regular patterns in traffic
        let mut periodic_indicators = 0;
        let window_size = samples.len() / 4;
        
        for i in 0..samples.len() - window_size {
            let current = samples[i].0;
            let future = samples[i + window_size].0;
            
            // If traffic pattern repeats with some regularity
            if (current - future).abs() < (current * 0.2) {
                periodic_indicators += 1;
            }
        }
        
        periodic_indicators as f64 / (samples.len() - window_size) as f64
    }
    
    /// Estimate connection count from traffic patterns
    fn estimate_connection_count(&self, avg_throughput: f64, max_throughput: f64) -> i64 {
        // Simple heuristic: higher throughput and burstiness suggests more connections
        let burst_factor = max_throughput / avg_throughput.max(0.001);
        let base_connections = (avg_throughput * 100.0) as i64; // Rough estimate
        let burst_multiplier = (burst_factor * 10.0) as i64;
        
        base_connections + burst_multiplier
    }
    
    /// Infer service type from traffic characteristics
    fn infer_service_type_from_traffic(&self, avg_throughput: f64, max_throughput: f64) -> String {
        if avg_throughput > 100.0 {
            "database".to_string() // High throughput suggests database
        } else if max_throughput / avg_throughput > 5.0 {
            "message_queue".to_string() // Bursty traffic suggests message queue
        } else if avg_throughput > 10.0 {
            "api_gateway".to_string() // Medium-high throughput suggests API gateway
        } else {
            "web_server".to_string() // Lower throughput suggests web server
        }
    }
    
    /// Calculate confidence score for traffic-based discovery
    fn calculate_traffic_confidence(
        &self,
        connections: i64,
        periodic_score: f64,
        throughput: f64,
    ) -> f64 {
        let connection_score = (connections as f64 / 1000.0).min(1.0);
        let periodic_weight = periodic_score * 0.4;
        let connection_weight = connection_score * 0.4;
        let throughput_weight = (throughput / 50.0).min(1.0) * 0.2;
        
        periodic_weight + connection_weight + throughput_weight
    }
    
    /// Store discovered service endpoint in graph
    pub fn store_service_endpoint(
        &self,
        conn: &Connection<'_>,
        endpoint: &ServiceEndpoint,
    ) -> Result<()> {
        conn.query(
            "MERGE (se:ServiceEndpoint {id: $id}) \
             SET se.device_address = $device_address, \
                 se.interface_name = $interface_name, \
                 se.service_type = $service_type, \
                 se.service_name = $service_name, \
                 se.endpoint_type = $endpoint_type, \
                 se.connection_count = $connection_count, \
                 se.avg_throughput_mbps = $avg_throughput_mbps, \
                 se.discovered_via = $discovered_via, \
                 se.confidence_score = $confidence_score, \
                 se.updated_at_ns = $updated_at_ns",
            vec![
                ("id", Value::String(endpoint.id.clone())),
                ("device_address", Value::String(endpoint.device_address.clone())),
                ("interface_name", Value::String(endpoint.interface_name.clone())),
                ("service_type", Value::String(endpoint.service_type.clone())),
                ("service_name", Value::String(endpoint.service_name.clone())),
                ("endpoint_type", Value::String(endpoint.endpoint_type.clone())),
                ("connection_count", Value::Int64(endpoint.connection_count)),
                ("avg_throughput_mbps", Value::Double(endpoint.avg_throughput_mbps)),
                ("discovered_via", Value::String(endpoint.discovered_via.clone())),
                ("confidence_score", Value::Double(endpoint.confidence_score)),
                ("updated_at_ns", Value::Int64(endpoint.updated_at_ns)),
            ],
        )
        .context("store service endpoint")?;
        
        // Create relationship to device
        conn.query(
            "MATCH (d:Device {address: $device_address}), (se:ServiceEndpoint {id: $id}) \
             MERGE (d)-[:HOSTS_SERVICE {role: $endpoint_type, updated_at: $updated_at}]->(se)",
            vec![
                ("device_address", Value::String(endpoint.device_address.clone())),
                ("id", Value::String(endpoint.id.clone())),
                ("endpoint_type", Value::String(endpoint.endpoint_type.clone())),
                ("updated_at", Value::Int64(now_ns())),
            ],
        )
        .context("create service endpoint relationship")?;
        
        Ok(())
    }
}

fn read_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        Value::Int32(n) => *n as i64,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn service_pattern_matching() {
        let config = ServiceDiscoveryConfig::default();
        let discovery = ServiceDiscovery::new(config);
        
        let endpoint = discovery.analyze_interface_description(
            "192.0.2.1",
            "ethernet-1/1",
            "API Gateway - Frontend",
            1_000_000_000,
        );
        
        assert!(endpoint.is_some());
        let endpoint = endpoint.unwrap();
        assert_eq!(endpoint.service_type, "api_gateway");
        assert!(endpoint.confidence_score > 0.8);
    }
    
    #[test]
    fn traffic_pattern_analysis() {
        let config = ServiceDiscoveryConfig::default();
        let discovery = ServiceDiscovery::new(config);
        
        // Create mock traffic data with periodic pattern
        let mut samples = Vec::new();
        let base_time = now_ns();
        for i in 0..100 {
            let throughput = 50.0 + (i % 10) as f64 * 5.0; // Periodic pattern
            samples.push((throughput, base_time + i * 1_000_000_000));
        }
        
        let endpoint = discovery.analyze_traffic_pattern(
            "192.0.2.1",
            "ethernet-1/2",
            &samples,
        );
        
        // Should detect some service based on traffic pattern
        assert!(endpoint.is_some());
    }
}
