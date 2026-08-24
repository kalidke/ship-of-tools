//! Frame envelope + class payloads (ADR 0039 normative schema).
//!
//! Closed enums use serde's default fail-on-unknown behavior — that IS the
//! fail-closed rule. Unknown *object members* are ignorable per the ADR, so
//! no `deny_unknown_fields` anywhere. u53 bounds are checked by `validate`
//! (serde_json parses into u64/i64; the bound is a schema rule, not a parse
//! rule).

use serde::{Deserialize, Serialize};

pub const U53_MAX: u64 = (1 << 53) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Seq {
    pub epoch: u64,
    pub n: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Emitter {
    Producer,
    Adapter,
    Capsule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Controller,
    Producer,
    AdapterPolicy,
    Foreign,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub take_epoch: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Derivation {
    Native,
    Synthetic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub emitter: Emitter,
    pub actor: Actor,
    pub derivation: Derivation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamMode {
    Append,
    Replace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stream {
    pub cell: String,
    pub mode: StreamMode,
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<Seq>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformKind {
    RedactField,
    ExtractBlob,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformOp {
    pub op: TransformKind,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transformed {
    pub ops: Vec<TransformOp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    RespondsTo,
    CausedBy,
    Revises,
    DuplicateOf,
    AttachedTo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameRef {
    pub kind: RefKind,
    pub frame: Seq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobRef {
    pub algo: String, // registry v1 = {"sha256"}; validate() enforces
    pub digest: String,
    pub length: u64,
    pub media_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Digest {
    pub algo: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadEncoding {
    Bytes,
    #[serde(rename = "json-utf8")]
    JsonUtf8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadRef {
    #[serde(flatten)]
    pub blob: BlobRef,
    pub encoding: PayloadEncoding,
}

/// The envelope. `payload` stays raw JSON here (class payloads and producer
/// payloads alike); typed views are below. Exactly one of `payload` /
/// `payload_ref` — enforced by `validate`, since serde can't express XOR
/// without a tagged union that would leak into the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: Seq,
    pub class: Class,
    pub source: Source,
    pub t_wall_ms: i64,
    pub t_mono_us: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<Stream>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transformed: Option<Transformed>,
    pub refs: Vec<FrameRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_ref: Option<PayloadRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Input,
    TurnOpen,
    TurnClose,
    ControlExchange,
    ArtifactRef,
    Lifecycle,
    ProducerAttached,
    Producer,
}

// --- typed class payloads (parsed on demand from Envelope.payload) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputContent {
    Redacted,
    Inline { inline: String },
    Blob { blob: BlobRef },
}

// InputContent's wire form is "redacted" | {inline} | {blob} — a bare string
// or an object. Serde untagged handles it with a custom shape:
impl InputContent {
    pub fn from_value(v: &serde_json::Value) -> Option<Self> {
        match v {
            serde_json::Value::String(s) if s == "redacted" => Some(InputContent::Redacted),
            serde_json::Value::Object(m) => {
                if let Some(serde_json::Value::String(s)) = m.get("inline") {
                    Some(InputContent::Inline { inline: s.clone() })
                } else {
                    m.get("blob")
                        .and_then(|b| serde_json::from_value(b.clone()).ok())
                        .map(|blob| InputContent::Blob { blob })
                }
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnCloseReason {
    ProducerDone,
    TerminalRes,
    Interrupted,
    Failed,
    SynthesizedDeath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangePhase {
    Request,
    Response,
    Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleKind {
    ProducerSpawn,
    ProducerReady,
    ProducerDead,
    TakeState,
    CaptureOptin,
    InputFact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputFactKind {
    ForwardIntent,
    Forwarded,
    ProducerObserved,
    RefusedStaleEpoch,
}

impl Envelope {
    /// Schema checks serde cannot express: u53 bounds, payload XOR,
    /// closed algo registries, hex shapes. The deeper cross-field matrix
    /// lives in `verify` (it needs voyage context).
    pub fn validate(&self) -> crate::Result<()> {
        let schema = |m: String| crate::Error::Schema(m);
        for (name, v) in [
            ("seq.epoch", self.seq.epoch),
            ("seq.n", self.seq.n),
            ("t_mono_us", self.t_mono_us),
        ] {
            if v > U53_MAX {
                return Err(schema(format!("{name} exceeds u53")));
            }
        }
        if self.t_wall_ms.unsigned_abs() > U53_MAX {
            return Err(schema("t_wall_ms exceeds i53".into()));
        }
        if self.seq.n == 0 {
            return Err(schema("seq.n starts at 1".into()));
        }
        match (&self.payload, &self.payload_ref) {
            (Some(_), None) | (None, Some(_)) => {}
            _ => return Err(schema("exactly one of payload/payload_ref".into())),
        }
        if let Some(pr) = &self.payload_ref {
            validate_blob_ref(&pr.blob)?;
        }
        if let Some(te) = self.source.actor.take_epoch {
            if te > U53_MAX {
                return Err(schema("actor.take_epoch exceeds u53".into()));
            }
        }
        if let Some(s) = &self.stream {
            if s.mode == StreamMode::Append && s.complete {
                return Err(schema("stream append+complete is illegal".into()));
            }
        }
        Ok(())
    }
}

pub fn validate_blob_ref(b: &BlobRef) -> crate::Result<()> {
    if b.algo != "sha256" {
        return Err(crate::Error::Schema(format!(
            "unknown blob algo {:?}",
            b.algo
        )));
    }
    if b.digest.len() != 64 || !b.digest.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
        return Err(crate::Error::Schema("blob digest must be 64 lowercase hex".into()));
    }
    if b.length > U53_MAX {
        return Err(crate::Error::Schema("blob length exceeds u53".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal(class: Class) -> Envelope {
        Envelope {
            seq: Seq { epoch: 1, n: 1 },
            class,
            source: Source {
                emitter: Emitter::Capsule,
                actor: Actor {
                    kind: ActorKind::Unknown,
                    controller_id: None,
                    take_epoch: None,
                },
                derivation: Derivation::Synthetic,
            },
            t_wall_ms: 0,
            t_mono_us: 0,
            stream: None,
            transformed: None,
            refs: vec![],
            payload: Some(json!({})),
            payload_ref: None,
        }
    }

    #[test]
    fn payload_xor_enforced() {
        let mut e = minimal(Class::Producer);
        e.payload = None;
        assert!(e.validate().is_err());
        e.payload = Some(json!({}));
        e.payload_ref = Some(PayloadRef {
            blob: BlobRef {
                algo: "sha256".into(),
                digest: "0".repeat(64),
                length: 1,
                media_type: "application/json".into(),
            },
            encoding: PayloadEncoding::Bytes,
        });
        assert!(e.validate().is_err());
    }

    #[test]
    fn unknown_closed_enum_value_fails_closed() {
        let mut v = serde_json::to_value(minimal(Class::Producer)).unwrap();
        v["class"] = json!("brand_new_class");
        assert!(serde_json::from_value::<Envelope>(v).is_err());
    }

    #[test]
    fn unknown_object_members_are_ignorable() {
        let mut v = serde_json::to_value(minimal(Class::Producer)).unwrap();
        v["totally_new_field"] = json!({"x": 1});
        assert!(serde_json::from_value::<Envelope>(v).is_ok());
    }

    #[test]
    fn append_complete_is_illegal() {
        let mut e = minimal(Class::Producer);
        e.stream = Some(Stream {
            cell: "c".into(),
            mode: StreamMode::Append,
            complete: true,
            prev: None,
        });
        assert!(e.validate().is_err());
    }

    #[test]
    fn n_zero_rejected() {
        let mut e = minimal(Class::Producer);
        e.seq.n = 0;
        assert!(e.validate().is_err());
    }
}
