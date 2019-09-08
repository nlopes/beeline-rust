/// assumes a header of the form:
///
/// VERSION;PAYLOAD

/// VERSION=1
/// =========
/// PAYLOAD is a list of comma-separated params (k=v pairs), with no spaces.  recognized
/// keys + value types:
///
///  trace_id=${traceId}    - traceId is an opaque ascii string which shall not include ','
///  parent_id=${spanId}    - spanId is an opaque ascii string which shall not include ','
///  dataset=${datasetId}   - datasetId is the slug for the honeycomb dataset to which downstream spans should be sent; shall not include ','
///  context=${contextBlob} - contextBlob is a base64 encoded json object.
///
/// ex: X-Honeycomb-Trace: 1;trace_id=weofijwoeifj,parent_id=owefjoweifj,context=SGVsbG8gV29ybGQ=
use base64;
use serde_json::json;

use libhoney::Value;

const PROPAGATION_HTTP_HEADER: &str = "X-Honeycomb-Trace";
const PROPAGATION_VERSION: usize = 1;

/// Propagation contains all the information about a payload header
///  trace_id=${traceId}    - traceId is an opaque ascii string which shall not include ','
///  parent_id=${spanId}    - spanId is an opaque ascii string which shall not include ','
///  dataset=${datasetId}   - datasetId is the slug for the honeycomb dataset to which downstream spans should be sent; shall not include ','
///  context=${contextBlob} - contextBlob is a base64 encoded json object.
///
/// ex: X-Honeycomb-Trace: 1;trace_id=weofijwoeifj,parent_id=owefjoweifj,context=SGVsbG8gV29ybGQ=
#[derive(Debug, PartialEq)]
pub struct Propagation {
    pub trace_id: String,
    pub parent_id: String,
    pub dataset: String,
    pub trace_context: Value,
}

impl Propagation {
    pub fn unmarshal_trace_context(header: &str) -> Self {
        let ver: Vec<&str> = header.splitn(2, ';').collect();
        if ver[0] == "1" {
            return Propagation::unmarshal_trace_context_v1(ver[1]);
        }

        // TODO: this should be an error
        Self {
            trace_id: "".to_string(),
            parent_id: "".to_string(),
            dataset: "".to_string(),
            trace_context: json!({}),
        }
    }

    fn unmarshal_trace_context_v1(header: &str) -> Self {
        let clauses: Vec<&str> = header.split(',').collect();
        let (mut trace_id, mut parent_id, mut dataset, mut context) = (
            "".to_string(),
            "".to_string(),
            "".to_string(),
            "".to_string(),
        );

        for clause in clauses.iter() {
            let kv: Vec<&str> = clause.splitn(2, '=').collect();
            match kv[0] {
                "trace_id" => trace_id = kv[1].to_string(),
                "parent_id" => parent_id = kv[1].to_string(),
                "dataset" => dataset = kv[1].to_string(),
                "context" => context = kv[1].to_string(),
                _ => (),
            };
        }

        if trace_id.is_empty() && !parent_id.is_empty() {
            // TODO: return error
            unimplemented!()
        }

        Propagation {
            trace_id,
            parent_id,
            dataset,
            trace_context: serde_json::from_slice(&base64::decode(&context).unwrap()).unwrap(),
        }
    }

    pub fn marshal_trace_context(&self) -> String {
        let dataset = if self.dataset != "" {
            format!("dataset={},", self.dataset)
        } else {
            String::new()
        };

        format!(
            "{};trace_id={},parent_id={},{}context={}",
            PROPAGATION_VERSION,
            self.trace_id,
            self.parent_id,
            dataset,
            base64::encode(&self.trace_context.to_string())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_marshal() {
        let mut p = Propagation {
            trace_id: "abcdef123456".to_string(),
            parent_id: "0102030405".to_string(),
            trace_context: json!({
                "userID": 1,
                "errorMsg": "failed to sign on",
                "toRetry":  true,
            }),
            dataset: "".to_string(),
        };
        assert_eq!(
            p.marshal_trace_context(),
            "1;trace_id=abcdef123456,parent_id=0102030405,context=eyJlcnJvck1zZyI6ImZhaWxlZCB0byBzaWduIG9uIiwidG9SZXRyeSI6dHJ1ZSwidXNlcklEIjoxfQ=="
        );

        p.dataset = "dada".to_string();
        assert_eq!(
            p.marshal_trace_context(),
            "1;trace_id=abcdef123456,parent_id=0102030405,dataset=dada,context=eyJlcnJvck1zZyI6ImZhaWxlZCB0byBzaWduIG9uIiwidG9SZXRyeSI6dHJ1ZSwidXNlcklEIjoxfQ=="
        );
    }

    #[test]
    fn test_unmarshal_with_dataset() {
        let p = Propagation {
            trace_id: "weofijwoeifj".to_string(),
            parent_id: "owefjoweifj".to_string(),
            dataset: "dada".to_string(),
            trace_context: json!({"key": "value"}),
        };
        assert_eq!(
            p,
            Propagation::unmarshal_trace_context(&p.marshal_trace_context())
        );
    }

}
