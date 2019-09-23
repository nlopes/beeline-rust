use std::collections::HashMap;
use std::sync::Arc;

use log::error;
use parking_lot::Mutex;
use serde_json::json;
use uuid::Uuid;

use crate::propagation::Propagation;
use crate::timer::{self, Timing};
use crate::Client;

use libhoney::{Builder, Event, FieldHolder, Sender, Value};

pub type SafeSpan = Arc<Mutex<Span>>;
pub type SafeTrace = Arc<Mutex<Trace>>;

/// Trace holds some trace level state and the root of the span tree that will be the
/// entire in-process trace. Traces are sent to Honeycomb when the root span is sent. You
/// can send a trace manually, and that will cause all synchronous spans in the trace to be
/// sent and sent. Asynchronous spans must still be sent on their own
#[derive(Debug, Clone)]
pub struct Trace {
    builder: Builder,
    pub trace_id: String,
    parent_id: String,
    rollup_fields: HashMap<String, f64>,
    root_span: SafeSpan,
    trace_level_fields: Value,
    child_spans: HashMap<String, Span>,
}

/// Trait to be able to send the trace
pub trait TraceSender<T: Sender> {
    fn send(&self, client: &mut Client<T>);
}

/// Implement send for trait TraceSender
impl<T: Sender> TraceSender<T> for SafeTrace {
    fn send(&self, client: &mut Client<T>) {
        let trace = self.clone();
        let cloned = trace.lock().root_span.clone();
        let mut root_span = cloned.lock();
        if !root_span.is_sent {
            root_span.send(&mut *client);
        }
    }
}

impl Trace {
    // `new` creates a brand new trace. serialized_headers is optional, and if included,
    // should be the header as written by trace.serialize_headers(). When not starting
    // from an upstream trace, pass None here.
    pub(crate) fn new<T: Sender>(
        client: &Client<T>,
        serialized_headers: Option<String>,
    ) -> SafeTrace {
        let trace = Arc::new(Mutex::new(Self {
            builder: client.new_builder(),
            trace_id: String::new(),
            parent_id: String::new(),
            trace_level_fields: json!({}),
            root_span: Arc::new(Mutex::new(Span::new())),
            rollup_fields: HashMap::new(),
            child_spans: HashMap::new(),
        }));

        let cloned = trace.clone();
        let mut t = cloned.lock();

        if let Some(headers) = serialized_headers {
            let prop = Propagation::unmarshal_trace_context(&headers);
            // TODO: check for error and info error do the below:
            t.trace_id = prop.trace_id;
            t.parent_id = prop.parent_id;
            t.builder.options.dataset = prop.dataset;
            t.trace_level_fields = prop.trace_context;
        }

        if t.trace_id.is_empty() {
            t.trace_id = Uuid::new_v4().to_string();
        }

        let mut root_span = Span::new();
        root_span.is_root = true;
        if !t.parent_id.is_empty() {
            root_span.parent_id = t.parent_id.clone();
        }
        root_span.ev = Some(t.builder.new_event());
        root_span.trace = Some(t.trace_id.clone());
        t.root_span = Arc::new(Mutex::new(root_span));
        trace
    }

    /// `add_field` adds a field to the trace. Every span in the trace will have this
    /// field added to it. These fields are also passed along to downstream services.  It
    /// is useful to add fields here that pertain to the entire trace, to aid in filtering
    /// spans at many different areas of the trace together.
    pub fn add_field(&mut self, key: &str, value: Value) {
        if let Some(ref mut tlf) = self.trace_level_fields.as_object_mut() {
            tlf.insert(key.to_string(), value);
        }
    }

    /// `serialize_headers` returns the trace ID, given span ID as parent ID, and an
    /// encoded form of all trace level fields. This serialized header is intended to be
    /// put in an HTTP (or other protocol) header to transmit to downstream services so
    /// they may start a new trace that will be connected to this trace.  The serialized
    /// form may be passed to NewTrace() in order to create a new trace that will be
    /// connected to this trace.
    fn serialize_headers(&self, span_id: &str) -> String {
        Propagation {
            trace_id: self.trace_id.clone(),
            parent_id: span_id.to_string(),
            dataset: self.builder.options.dataset.clone(),
            trace_context: self.trace_level_fields.clone(),
        }
        .marshal_trace_context()
    }

    /// `add_rollup_field` is here to let a span contribute a field to the trace while
    /// keeping the trace's locks private.
    fn add_rollup_field(&mut self, key: &str, value: f64) {
        let v = self.rollup_fields.entry(key.to_string()).or_insert(0f64);
        *v += value;
    }

    pub fn get_root_span(&mut self) -> SafeSpan {
        self.root_span.clone()
    }

    /// `remove_child_span`
    pub(crate) fn remove_child_span(&mut self, span_id: String) {
        self.child_spans.remove(&span_id);
    }
}

#[derive(Debug, Default, Clone)]
pub struct Span {
    is_async: bool,
    is_sent: bool,
    is_root: bool,
    children: Vec<SafeSpan>,
    ev: Option<Event>,
    span_id: String,
    parent_id: String,
    rollup_fields: Arc<Mutex<HashMap<String, f64>>>,
    timer: timer::Timer,
    trace: Option<String>,
}

impl Span {
    fn new() -> Span {
        Self {
            span_id: Uuid::new_v4().to_string(),
            ..Default::default()
        }
    }

    /// `add_field` adds a key/value pair to this span
    pub fn add_field(&mut self, key: &str, value: Value) {
        if let Some(ref mut ev) = self.ev {
            ev.add_field(key, value);
        }
    }

    /// `get_children` returns a list of all child spans (both synchronous and
    /// asynchronous).
    pub fn get_children(&self) -> Vec<SafeSpan> {
        self.children.to_vec()
    }

    pub fn send<T: Sender>(&mut self, client: &mut Client<T>) {
        if !self.is_sent {
            self.send_locked(client);
        }
    }

    fn send_by_parent<T: Sender>(&mut self, client: &mut Client<T>) {
        if !self.is_sent {
            self.add_field("meta.sent_by_parent", json!(true));
            self.send_locked(client);
        }
    }

    fn send_locked<T: Sender>(&mut self, client: &mut Client<T>) {
        if self.ev.is_none() {
            return;
        }

        // finish the timer for this span
        self.add_field("duration_ms", json!(self.timer.finish() as u64)); // TODO: dangerous

        if !self.parent_id.is_empty() {
            self.add_field("trace.parent_id", json!(self.parent_id.clone()));
        }

        if let Some(ref mut ev) = self.ev {
            // set trace IDs for this span
            if let Some(ref trace_id) = self.trace {
                ev.add_field("trace.trace_id", json!(trace_id));
            }
            ev.add_field("trace.span_id", json!(self.span_id.clone()));
        }

        // add this span's rollup fields to the event
        for (k, v) in self.rollup_fields.clone().lock().iter() {
            self.add_field(k, json!(v));
        }

        let mut children: Vec<SafeSpan> = Vec::new();

        for v in self.children.iter() {
            if !v.lock().is_async {
                // queue children up to be sent. We'd deadlock if we actually sent the
                // child here.
                children.push(v.clone());
            }
        }

        for child in children.iter_mut() {
            child.lock().send_by_parent(client);
        }

        self.final_send(client);
        self.is_sent = true;

        if let Some(ref trace_id) = self.trace {
            client.remove_child_span_from_trace(trace_id.to_string(), self.span_id.clone());
        }
    }

    /// send gets all the trace level fields and does pre-send hooks, then sends the span.
    fn final_send<T: Sender>(&mut self, client: &mut Client<T>) {
        // add all the trace level fields to the event as late as possible - when the
        // trace is all getting sent
        if let Some(trace_id) = &self.trace {
            if let Some(trace) = client.get_trace(trace_id.to_string()) {
                if let Some(fields) = trace.lock().trace_level_fields.clone().as_object() {
                    for (k, v) in fields.into_iter() {
                        self.add_field(k, v.clone());
                    }
                }
            }
        }

        let span_type = if self.is_root {
            if self.parent_id.is_empty() {
                "root"
            } else {
                "subroot"
            }
        } else if self.is_async {
            "async"
        } else if self.children.is_empty() {
            "leaf"
        } else {
            "mid"
        };

        self.add_field("meta.span_type", Value::String(span_type.to_string()));
        if span_type == "root" {
            for (k, v) in self.rollup_fields.clone().lock().iter() {
                self.add_field(&format!("rollup.{}", k), json!(v))
            }
        }
        if let Some(ref mut ev) = self.ev {
            let sampler_hook = client.0.clone().read().config.sampler_hook.clone();
            let (should_keep, sample_rate) = sampler_hook(ev.fields());
            ev.set_sample_rate(sample_rate);

            if should_keep {
                let presend_hook = client.0.clone().read().config.presend_hook.clone();
                let presend_hook = &mut *presend_hook.lock();
                presend_hook(ev.get_fields_mut());

                if let Err(e) = ev.send_presampled(&mut client.0.write().client) {
                    error!("Error sending event: {}", e);
                }
            }
        }
    }

    /// `create_async_child` creates a child of the current span that is expected to
    /// outlive the current span (and trace). Async spans are not automatically sent when
    /// their parent finishes, but are otherwise identical to synchronous spans.
    pub fn create_async_child<T: Sender>(&mut self, client: &mut Client<T>) -> Option<SafeSpan> {
        self.create_child_span(client, true)
    }

    /// Span creates a synchronous child of the current span. Spans must finish before
    /// their parents.
    pub fn create_child<T: Sender>(&mut self, client: &mut Client<T>) -> Option<SafeSpan> {
        self.create_child_span(client, false)
    }

    /// `serialize_headers` returns the trace ID, current span ID as parent ID, and an
    /// encoded form of all trace level fields. This serialized header is intended to be
    /// put in an HTTP (or other protocol) header to transmit to downstream services so
    /// they may start a new trace that will be connected to this trace.  The serialized
    /// form may be passed to NewTrace() in order to create a new trace that will be
    /// connected to this trace.
    pub fn serialize_headers<T: Sender>(&self, client: &mut Client<T>) -> String {
        match &self.trace {
            Some(trace_id) => match client.get_trace(trace_id.to_string()) {
                Some(trace) => trace.lock().serialize_headers(&self.span_id),
                None => "".to_string(),
            },
            None => "".to_string(),
        }
    }

    fn create_child_span<T: Sender>(
        &mut self,
        client: &mut Client<T>,
        is_async: bool,
    ) -> Option<SafeSpan> {
        if let Some(trace_id) = &self.trace {
            let span_id = Uuid::new_v4().to_string();
            let ev = if let Some(trace) = client.get_trace(trace_id.to_string()) {
                Some(trace.lock().builder.new_event())
            } else {
                None
            };
            let new_span = Span {
                span_id: span_id.clone(),
                parent_id: self.span_id.clone(),
                trace: Some(trace_id.to_string()),
                ev,
                is_async,
                ..Default::default()
            };
            let span = Arc::new(Mutex::new(new_span));
            self.children.push(span.clone());
            if let Some(trace) = client.get_trace(trace_id.to_string()) {
                trace
                    .lock()
                    .child_spans
                    .insert(span_id, (*span).lock().clone());
                Some(span)
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::tests::new_client;
    use crate::Config;

    #[test]
    fn test_new_span() {
        let span = Span::new();
        assert_eq!(span.span_id.len(), 36);
        assert_eq!(span.get_children().len(), 0);
    }

    #[test]
    fn test_new_trace() {
        let client = new_client(Config::default());
        let cloned = Trace::new(&client, None);
        let trace = cloned.lock();
        assert!(!trace.trace_id.is_empty());
        assert!(trace.parent_id.is_empty());
        assert!(trace.rollup_fields.is_empty());
        assert_eq!(trace.trace_level_fields, json!({}));
        assert_eq!(trace.root_span.lock().is_root, true);
    }

    #[test]
    fn test_new_trace_with_serialized_headers() {
        let client = new_client(Config::default());
        let serialized_headers = "1;trace_id=weofijwoeifj,parent_id=owefjoweifj,context=eyJlcnJvck1zZyI6ImZhaWxlZCB0byBzaWduIG9uIiwidG9SZXRyeSI6dHJ1ZSwidXNlcklEIjoxfQ==".to_string();
        let cloned = Trace::new(&client, Some(serialized_headers));
        let trace = cloned.lock();

        assert_eq!(trace.trace_id, "weofijwoeifj");
        assert_eq!(trace.parent_id, "owefjoweifj");

        match trace.trace_level_fields.as_object() {
            Some(tlf) => {
                assert_eq!(tlf["userID"], json!(1));
                assert_eq!(tlf["toRetry"], json!(true));
                assert_eq!(tlf["errorMsg"], json!("failed to sign on"));
            }
            None => panic!("expected fields from serialized headers"),
        };
    }

    #[test]
    fn test_trace_add_field() {
        let client = new_client(Config::default());
        let cloned = Trace::new(&client, None);
        let mut trace = cloned.lock();
        assert!(trace.trace_level_fields.is_object());
        trace.add_field("nor", json!({"a": 1}));
        match trace.trace_level_fields.as_object() {
            Some(tlf) => assert_eq!(tlf["nor"], json!({"a": 1})),
            None => panic!("expected field"),
        };
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_trace_rollup_fields() {
        let client = new_client(Config::default());
        let cloned = Trace::new(&client, None);
        let mut trace = cloned.lock();
        trace.add_rollup_field("bignum", 5.0f64);
        trace.add_rollup_field("bignum", 5.0f64);
        trace.add_rollup_field("smallnum", 0.1f64);

        assert_eq!(trace.rollup_fields["bignum"], 10f64);
        assert_eq!(trace.rollup_fields["smallnum"], 0.1f64);
    }

    #[test]
    fn test_send_trace() {
        let mut client = new_client(Config::default());
        let trace = client.new_trace(None);
        {
            let rs = trace.lock().get_root_span();
            let mut rs_guard = rs.lock();
            rs_guard.add_field("name", Value::String("rs".to_string()));

            let c1 = rs_guard.create_child(&mut client).unwrap();
            c1.lock().add_field("name", Value::String("c1".to_string()));
            let c2 = c1.lock().create_child(&mut client).unwrap();
            c2.lock().add_field("name", Value::String("c2".to_string()));
            let ac1 = c1.lock().create_async_child(&mut client).unwrap();
            ac1.lock()
                .add_field("name", Value::String("ac1".to_string()));

            let not_sent_child = ac1.lock().create_child(&mut client).unwrap();
            not_sent_child
                .lock()
                .add_field("name", Value::String("not_sent_child".to_string()));
        }
        trace.send(&mut client);
        let events = client.0.write().client.transmission.events();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_send_trace_prehook() {
        let mut config = crate::Config::default();

        // This variable gets set to true within the presend_hook. That way, we can then
        // test that the presend_hook was in fact run internally.
        let presend_hook_ran = Arc::new(Mutex::new(false));
        let presend_hook_ran_inner = presend_hook_ran.clone();
        config.presend_hook = Arc::new(Mutex::new(
            move |e: &mut HashMap<String, libhoney::Value>| {
                let mut ran = presend_hook_ran_inner.lock();
                *ran = true;
                e.clear();
            },
        ));
        let mut client = new_client(config);

        let trace = client.new_trace(None);
        {
            let rs = trace.lock().get_root_span();
            let mut rs_guard = rs.lock();
            rs_guard.add_field("name", Value::String("rs".to_string()));
        }
        trace.send(&mut client);
        assert!(*presend_hook_ran.lock());
    }

    #[test]
    fn test_send_trace_sampler_hook() {
        let mut config = crate::Config::default();
        config.sampler_hook = Arc::new(|_| (false, 1));
        let mut client = new_client(config);

        let trace = client.new_trace(None);
        {
            let rs = trace.lock().get_root_span();
            let mut rs_guard = rs.lock();
            rs_guard.add_field("name", Value::String("rs".to_string()));
        }
        trace.send(&mut client);
        let events = client.0.write().client.transmission.events();
        // This ends up being true because we set the sampler_hook to drop the event
        assert!(events.is_empty())
    }
}
