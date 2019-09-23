/*! Easy instrumentation for rust apps with Honeycomb

Please do **not** use in production (yet). I'm still experimenting with the right interface to the library so everything can break.
If you do give this a go and have ideas on library ergonomics please raise an issue with ideas.

*/
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

mod propagation;
mod timer;
pub mod trace;

pub use libhoney::client::Options as ClientOptions;
pub use libhoney::transmission::Options as TransmissionOptions;
pub use libhoney::Config as ClientConfig;
pub use libhoney::{transmission::Transmission, Sender};

pub use trace::{SafeTrace, Trace};

#[derive(Clone)]
pub struct Config {
    pub client_config: ClientConfig,
    pub service_name: Option<String>,
    pub sampler_hook:
        Arc<dyn Fn(HashMap<String, libhoney::Value>) -> (bool, usize) + 'static + Send + Sync>,
    pub presend_hook:
        Arc<Mutex<dyn FnMut(&mut HashMap<String, libhoney::Value>) + 'static + Send + Sync>>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Config {{\n  client_config: {:?},\n  service_name: {:?},\n  sampler_hook: Fn(),\n}}",
            self.client_config, self.service_name
        )
    }
}

impl Default for Config {
    fn default() -> Self {
        fn default_presend_hook(_ev: &mut HashMap<String, libhoney::Value>) {}

        Self {
            client_config: ClientConfig {
                options: ClientOptions {
                    api_key: "api-key-placeholder".to_string(),
                    dataset: "beeline-rust".to_string(),
                    sample_rate: 1,
                    ..libhoney::client::Options::default()
                },
                transmission_options: libhoney::transmission::Options::default(),
            },
            service_name: None,
            sampler_hook: Arc::new(|_| (true, 1)),
            presend_hook: Arc::new(Mutex::new(default_presend_hook)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Client<T: Sender>(pub Arc<RwLock<BeelineClient<T>>>);

#[derive(Debug, Clone)]
pub struct BeelineClient<T: Sender> {
    pub config: Config,
    pub client: libhoney::Client<T>,
    pub traces: Arc<Mutex<HashMap<String, SafeTrace>>>,
}

impl<T> Client<T>
where
    T: Sender,
{
    pub fn get_trace(&self, trace_id: String) -> Option<SafeTrace> {
        let traces = &self.0.write().traces.clone();
        let guard = traces.lock();
        match guard.get(&trace_id) {
            Some(trace) => Some(trace.clone()),
            None => None,
        }
    }

    pub fn remove_child_span_from_trace(&self, trace_id: String, span_id: String) {
        let traces = &self.0.write().traces;
        let guard = traces.lock();
        if let Some(trace) = guard.get(&trace_id) {
            let mut trace = trace.lock();
            trace.remove_child_span(span_id);
        }
    }

    pub fn new_builder(&self) -> libhoney::Builder {
        self.0.write().client.new_builder()
    }

    pub fn add_field(&mut self, name: &str, value: libhoney::Value) {
        self.0.write().client.add_field(name, value)
    }

    pub fn new_trace(&self, serialized_headers: Option<String>) -> SafeTrace {
        let trace = Trace::new(self, serialized_headers);
        self.0
            .write()
            .traces
            .lock()
            .insert(trace.lock().trace_id.clone(), trace.clone());
        trace
    }
}

pub fn init(config: Config) -> Client<Transmission> {
    let cfg = config.clone();
    let mut client: libhoney::client::Client<Transmission> = libhoney::init(cfg.client_config);

    internal_config::<Transmission>(config.clone(), &mut client);

    Client(Arc::new(RwLock::new(BeelineClient {
        config,
        client,
        traces: Arc::new(Mutex::new(HashMap::new())),
    })))
}

fn internal_config<T: Sender>(config: Config, client: &mut libhoney::Client<T>) {
    client.add_field(
        "meta.beeline_version",
        libhoney::Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );

    if let Some(svc) = config.service_name {
        client.add_field("meta.service_name", libhoney::Value::String(svc));
    }

    if let Some(hostname) = hostname::get_hostname() {
        client.add_field("meta.local_hostname", libhoney::Value::String(hostname));
    }
}

pub mod test {
    pub use libhoney::mock::TransmissionMock;

    use crate::{Client, Config};

    use super::*;

    pub fn init(config: Config) -> Client<TransmissionMock> {
        let cfg = config.clone();
        let mut client = libhoney::test::init(cfg.client_config);

        internal_config::<TransmissionMock>(config.clone(), &mut client);

        Client(Arc::new(RwLock::new(BeelineClient {
            config,
            client,
            traces: Arc::new(Mutex::new(HashMap::new())),
        })))
    }
}

#[cfg(test)]
mod tests {
    use libhoney::mock::TransmissionMock;
    use mockito;

    use super::*;
    use crate::trace::TraceSender;

    pub fn new_client(config: Config) -> Client<TransmissionMock> {
        let api_host = &mockito::server_url();
        let _m = mockito::mock(
            "POST",
            mockito::Matcher::Regex(r"/1/batch/(.*)$".to_string()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("[{ \"status\": 202 }]")
        .create();

        let mut config = config;
        config.client_config.options.api_host = api_host.to_string();
        config.service_name = Some("beeline-rust-test".to_string());
        crate::test::init(config)
    }

    #[test]
    fn test_multiple_threads_with_span() {
        let client = new_client(Config::default());
        let t1_trace = client.new_trace(None);
        let mut c1_client = client.clone();
        let t1 = std::thread::spawn(move || {
            {
                let rs = t1_trace.lock().get_root_span();
                {
                    let mut trace = t1_trace.lock();
                    trace.add_field("thread", serde_json::Value::String("one".to_string()));
                }
                let mut span_client = c1_client.clone();
                let mut root_span_guard = rs.lock();
                if let Some(new_span) = root_span_guard.create_child(&mut span_client) {
                    let mut new_span_guard = new_span.lock();
                    new_span_guard
                        .add_field("span", serde_json::Value::String("span_one".to_string()));
                    new_span_guard.send(&mut span_client);
                }
            }
            t1_trace.send(&mut c1_client);
        });

        let t2_trace = client.new_trace(None);
        let mut c2_client = client.clone();
        let t2 = std::thread::spawn(move || {
            {
                let mut trace = t2_trace.lock();
                trace.add_field("thread", serde_json::Value::String("two".to_string()));
            }
            t2_trace.send(&mut c2_client);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let events = client.0.write().client.transmission.events();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_multiple_threads() {
        let client = new_client(Config::default());
        let t1_trace = client.new_trace(None);
        let mut c1_client = client.clone();
        let t1 = std::thread::spawn(move || {
            {
                let mut trace = t1_trace.lock();
                trace.add_field("thread", serde_json::Value::String("one".to_string()));
            }
            t1_trace.send(&mut c1_client);
        });

        let t2_trace = client.new_trace(None);
        let mut c2_client = client.clone();
        let t2 = std::thread::spawn(move || {
            {
                let mut trace = t2_trace.lock();
                trace.add_field("thread", serde_json::Value::String("two".to_string()));
            }
            t2_trace.send(&mut c2_client);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let events = client.0.write().client.transmission.events();
        assert_eq!(events.len(), 2);
    }
}
