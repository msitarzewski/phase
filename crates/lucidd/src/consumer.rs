// SPDX-License-Identifier: AGPL-3.0-or-later

//! Private local control and delegated enrollment for LUCID consumers.
//!
//! Invitations are explicit operator grants. The invitation's delegation seed
//! is given privately to its recipient; only public keys and signed requests
//! cross the existing DHT. Enrollment never enables anonymous execution and
//! never bypasses the router's signer/transport, model, or resource checks.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use futures::stream::{self, StreamExt};
use phase_identity::NodeIdentity;
use phase_net::PeerId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;

use crate::policy::PolicyEngine;
use crate::registry::{peer_id_from_ed25519_pubkey, DhtTransport};

const SCHEMA: u32 = 1;
const MAX_INVITATION_BYTES: usize = 8 * 1024;
const MAX_RECORD_BYTES: usize = 8 * 1024;
const MAX_RECORDS: usize = 8;
const MAX_PENDING_INVITES: usize = 8;
const MAX_GRANTS: usize = 1024;
const MAX_STORE_BYTES: usize = 1024 * 1024;
const MAX_INVITE_TTL_SECS: u64 = 3600;
const GRANT_TTL_SECS: u64 = 24 * 3600;
// Must exceed phase-net's 10s DISCOVERY_OPERATION_TIMEOUT so WAN Kademlia
// PutRecord/GetRecord calls across NATted or --outbound-only fleets flush
// collected records rather than timing out prematurely.
const QUERY_DEADLINE: Duration = Duration::from_secs(15);
const ENROLLMENT_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const INVITE_DOMAIN: &str = "phase/lucid-enrollment/invitation/v1";
const REQUEST_DOMAIN: &str = "phase/lucid-enrollment/request/v1";
const APPROVAL_DOMAIN: &str = "phase/lucid-enrollment/approval/v1";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitationDescriptor {
    schema: u32,
    issuer: String,
    id: String,
    delegation_key: String,
    expires_at: u64,
    grant_expires_at: u64,
}

// No Debug implementation: this value contains the private delegation seed.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invitation {
    descriptor: InvitationDescriptor,
    issuer_signature: String,
    delegation_seed: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBody {
    schema: u32,
    issuer: String,
    invitation_id: String,
    requester: String,
    expires_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentRequest {
    body: RequestBody,
    delegation_signature: String,
    requester_signature: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalBody {
    schema: u32,
    issuer: String,
    invitation_id: String,
    requester: String,
    request_hash: String,
    expires_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    body: ApprovalBody,
    issuer_signature: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IssuedInvitation {
    descriptor: InvitationDescriptor,
    claimed_requester: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema: u32,
    owner: String,
    issued: Vec<IssuedInvitation>,
    grants: Vec<ApprovalBody>,
    received: Vec<Approval>,
}

impl Ledger {
    fn active_issued_grants(&self) -> impl Iterator<Item = &ApprovalBody> {
        let now = unix_seconds();
        self.grants
            .iter()
            .filter(move |grant| grant.issuer == self.owner && grant.expires_at > now)
    }
}

/// One atomic ledger holds invitation consumption and its resulting grant.
/// Policy readers consult only grants issued by this ledger's own node.
pub struct EnrollmentStore {
    path: PathBuf,
    ledger: Mutex<Ledger>,
}

impl EnrollmentStore {
    fn open(path: PathBuf, owner: String) -> Result<Self> {
        ensure_parent(&path)?;
        let ledger = match read_private(&path, MAX_STORE_BYTES)? {
            Some(bytes) => {
                let ledger: Ledger =
                    serde_json::from_slice(&bytes).context("invalid enrollment ledger")?;
                if ledger.schema != SCHEMA || ledger.owner != owner {
                    bail!("enrollment ledger does not match the node identity");
                }
                validate_ledger(&ledger)?;
                ledger
            }
            None => Ledger {
                schema: SCHEMA,
                owner,
                issued: Vec::new(),
                grants: Vec::new(),
                received: Vec::new(),
            },
        };
        Ok(Self {
            path,
            ledger: Mutex::new(ledger),
        })
    }

    /// Expired grants cannot authorize a request, including after restart.
    pub fn is_authorized(&self, requester: &str) -> bool {
        let Ok(ledger) = self.ledger.lock() else {
            return false;
        };
        let mut grants = ledger.active_issued_grants();
        grants.any(|grant| grant.requester.eq_ignore_ascii_case(requester))
    }

    fn persist(&self, ledger: &Ledger) -> Result<()> {
        let bytes = serde_json::to_vec(ledger)?;
        if bytes.len() > MAX_STORE_BYTES {
            bail!("enrollment ledger size limit reached")
        }
        atomic_private_write(&self.path, &bytes)
    }

    fn register_invitation(&self, descriptor: InvitationDescriptor) -> Result<()> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| anyhow!("enrollment ledger unavailable"))?;
        let mut next = ledger.clone();
        let now = unix_seconds();
        next.issued.retain(|x| x.descriptor.expires_at > now);
        next.grants.retain(|x| x.expires_at > now);
        next.received.retain(|x| x.body.expires_at > now);
        if next.issued.len() >= MAX_PENDING_INVITES {
            bail!("outstanding invitation limit reached")
        }
        next.issued.push(IssuedInvitation {
            descriptor,
            claimed_requester: None,
        });
        self.persist(&next)?;
        *ledger = next;
        Ok(())
    }

    fn pending_invitations(&self) -> Vec<InvitationDescriptor> {
        let Ok(ledger) = self.ledger.lock() else {
            return Vec::new();
        };
        let now = unix_seconds();
        ledger
            .issued
            .iter()
            .filter(|x| x.descriptor.expires_at > now)
            .take(MAX_PENDING_INVITES)
            .map(|x| x.descriptor.clone())
            .collect()
    }

    fn claim(&self, request: &EnrollmentRequest, now: u64) -> Result<ApprovalBody> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| anyhow!("enrollment ledger unavailable"))?;
        let mut next = ledger.clone();
        let invitation = next
            .issued
            .iter_mut()
            .find(|x| x.descriptor.id == request.body.invitation_id)
            .ok_or_else(|| anyhow!("invitation is not recognized"))?;
        validate_request(request, &invitation.descriptor, now)?;
        if invitation
            .claimed_requester
            .as_ref()
            .is_some_and(|key| key != &request.body.requester)
        {
            bail!("invitation has already been claimed");
        }
        let request_hash = request_hash(request)?;
        if let Some(grant) = next.grants.iter().find(|g| g.request_hash == request_hash) {
            return Ok(grant.clone());
        }
        next.grants.retain(|x| x.expires_at > now);
        if next.grants.len() >= MAX_GRANTS {
            bail!("enrollment grant limit reached")
        }
        invitation.claimed_requester = Some(request.body.requester.clone());
        let grant = ApprovalBody {
            schema: SCHEMA,
            issuer: invitation.descriptor.issuer.clone(),
            invitation_id: invitation.descriptor.id.clone(),
            requester: request.body.requester.clone(),
            request_hash,
            expires_at: invitation.descriptor.grant_expires_at,
        };
        next.grants.push(grant.clone());
        self.persist(&next)?;
        *ledger = next;
        Ok(grant)
    }

    fn accept_approval(&self, approval: Approval) -> Result<()> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| anyhow!("enrollment ledger unavailable"))?;
        let mut next = ledger.clone();
        next.received.retain(|x| x.body.expires_at > unix_seconds());
        next.received
            .retain(|x| x.body.issuer != approval.body.issuer);
        if next.received.len() >= MAX_GRANTS {
            bail!("enrollment grant limit reached")
        }
        next.received.push(approval);
        self.persist(&next)?;
        *ledger = next;
        Ok(())
    }

    fn has_received_grant(&self) -> bool {
        self.ledger.lock().ok().is_some_and(|l| {
            l.received
                .iter()
                .any(|a| a.body.expires_at > unix_seconds())
        })
    }

    /// A client may distinguish verified membership from mere discovery.
    pub fn authorized_publisher(&self, public_key: &[u8; 32]) -> bool {
        let issuer = hex(public_key);
        self.ledger.lock().ok().is_some_and(|l| {
            l.received
                .iter()
                .any(|a| a.body.issuer == issuer && a.body.expires_at > unix_seconds())
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EnrollmentStatus {
    pub state: String,
    pub message: String,
}

impl EnrollmentStatus {
    fn new(state: &str, message: &str) -> Self {
        Self {
            state: state.into(),
            message: message.into(),
        }
    }
}

#[derive(Serialize)]
pub struct InvitationResponse {
    /// Sensitive bearer invitation: only return to an authenticated local operator.
    pub invitation: String,
    pub expires_at: u64,
    pub grant_expires_at: u64,
}

/// Local app-control state. Tokens and invitation seeds have no Debug output.
pub struct ConsumerControl {
    identity: NodeIdentity,
    transport: Arc<dyn DhtTransport>,
    store: Arc<EnrollmentStore>,
    token: String,
    enrollment: RwLock<EnrollmentStatus>,
    enrolling: std::sync::atomic::AtomicBool,
}

/// One permit owns the in-flight flag, including across cancelled futures.
/// It moves from the publishing future into the approval task on success.
struct EnrollmentPermit(std::sync::Weak<ConsumerControl>);

impl Drop for EnrollmentPermit {
    fn drop(&mut self) {
        if let Some(control) = self.0.upgrade() {
            control
                .enrolling
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

impl ConsumerControl {
    /// Load private control state and attach this node's durable operator grants.
    pub fn open(
        identity: NodeIdentity,
        transport: Arc<dyn DhtTransport>,
        policy: Arc<PolicyEngine>,
        ledger_path: PathBuf,
        token_path: PathBuf,
    ) -> Result<Arc<Self>> {
        let token = control_token(&token_path)?;
        let store = Arc::new(EnrollmentStore::open(
            ledger_path,
            hex(&identity.peer_id_bytes()),
        )?);
        let initial = if store.has_received_grant() {
            EnrollmentStatus::new("authorized", "Enrollment is active.")
        } else {
            EnrollmentStatus::new(
                "idle",
                "Join with an operator invitation to request access.",
            )
        };
        policy.attach_enrollment_store(Arc::clone(&store));
        Ok(Arc::new(Self {
            identity,
            transport,
            store,
            token,
            enrollment: RwLock::new(initial),
            enrolling: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// The token is compared without an early per-byte mismatch return.
    pub fn authorize_bearer(&self, authorization: &str) -> bool {
        let Some(value) = authorization.strip_prefix("Bearer ") else {
            return false;
        };
        if value.len() != self.token.len() {
            return false;
        }
        value
            .bytes()
            .zip(self.token.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
    }

    pub fn enrollment_status(&self) -> EnrollmentStatus {
        let mut status = self
            .enrollment
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| {
                EnrollmentStatus::new("failed", "Enrollment state is unavailable.")
            });
        if status.state == "authorized" && !self.store.has_received_grant() {
            status =
                EnrollmentStatus::new("failed", "Enrollment expired. Request a new invitation.");
        }
        status
    }

    pub fn is_enrolled_with(&self, publisher: &[u8; 32]) -> bool {
        self.store.authorized_publisher(publisher)
    }

    pub fn authorized_issuers(&self) -> Vec<[u8; 32]> {
        let Ok(ledger) = self.store.ledger.lock() else {
            return Vec::new();
        };
        let now = unix_seconds();
        ledger
            .received
            .iter()
            .filter(|a| a.body.expires_at > now)
            .filter_map(|a| decode_key(&a.body.issuer).ok())
            .collect()
    }

    /// Network peers identified by the current, persisted enrollment grants.
    pub(crate) fn authorized_issuer_peers(&self) -> impl Iterator<Item = PeerId> {
        self.authorized_issuers()
            .into_iter()
            .filter_map(|issuer| peer_id_from_ed25519_pubkey(&issuer).ok())
    }

    /// Circle membership includes active invitations and explicit submitter
    /// allowlist entries. The development override does not enroll a peer.
    pub(crate) fn authorized_circle_peers(&self, policy: &PolicyEngine) -> HashSet<PeerId> {
        let mut peers: HashSet<_> = self.authorized_issuer_peers().collect();
        let peer_from_key = |key: &str| {
            // The operator's allowlist accepts either case. Signed enrollment
            // data still goes through the ledger's canonical decoder.
            let mut raw = [0; 32];
            hex::decode_to_slice(key, &mut raw).ok()?;
            peer_id_from_ed25519_pubkey(&raw).ok()
        };
        peers.extend(
            policy
                .config()
                .authorized_submitters
                .iter()
                .filter_map(|key| peer_from_key(key)),
        );
        if let Ok(ledger) = self.store.ledger.lock() {
            peers.extend(
                ledger
                    .active_issued_grants()
                    .filter_map(|grant| peer_from_key(&grant.requester)),
            );
        }
        peers
    }

    fn set_status(&self, state: &str, message: &str) {
        if let Ok(mut status) = self.enrollment.write() {
            *status = EnrollmentStatus::new(state, message);
        }
    }

    /// Create an invitation only on an authenticated local operator action.
    /// No delegation seed is retained in the ledger.
    pub async fn create_invitation(&self, ttl_seconds: u64) -> Result<InvitationResponse> {
        if !(1..=MAX_INVITE_TTL_SECS).contains(&ttl_seconds) {
            bail!("invitation lifetime must be within 1..=3600 seconds");
        }
        let now = unix_seconds();
        let delegation = NodeIdentity::generate();
        let descriptor = InvitationDescriptor {
            schema: SCHEMA,
            issuer: hex(&self.identity.peer_id_bytes()),
            id: uuid::Uuid::new_v4().simple().to_string(),
            delegation_key: hex(&delegation.peer_id_bytes()),
            expires_at: now + ttl_seconds,
            grant_expires_at: now + GRANT_TTL_SECS,
        };
        let invitation = Invitation {
            issuer_signature: sign(&self.identity, INVITE_DOMAIN, &descriptor)?,
            descriptor: descriptor.clone(),
            delegation_seed: hex(&delegation.signing_key().to_bytes()),
        };
        self.store.register_invitation(descriptor)?;
        let bytes = serde_json::to_vec(&invitation)?;
        Ok(InvitationResponse {
            expires_at: invitation.descriptor.expires_at,
            grant_expires_at: invitation.descriptor.grant_expires_at,
            invitation: format!("lucid://join/{}", URL_SAFE_NO_PAD.encode(bytes)),
        })
    }

    /// Publish a doubly signed request, return pending promptly, and wait for
    /// the pinned issuer's exact-request approval in a bounded background task.
    pub async fn enroll(self: &Arc<Self>, encoded: &str) -> Result<EnrollmentStatus> {
        let (invitation, delegation) = parse_invitation(encoded, unix_seconds())?;
        let body = RequestBody {
            schema: SCHEMA,
            issuer: invitation.descriptor.issuer.clone(),
            invitation_id: invitation.descriptor.id.clone(),
            requester: hex(&self.identity.peer_id_bytes()),
            expires_at: invitation.descriptor.expires_at,
        };
        let bytes = domain_bytes(REQUEST_DOMAIN, &body)?;
        let request = EnrollmentRequest {
            body,
            delegation_signature: hex(&delegation.sign(&bytes).to_bytes()),
            requester_signature: hex(&self.identity.sign(&bytes).to_bytes()),
        };
        use std::sync::atomic::Ordering;
        if self
            .enrolling
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            bail!("an enrollment request is already pending");
        }
        let permit = EnrollmentPermit(Arc::downgrade(self));
        let value = serde_json::to_vec(&request)?;
        let key = request_key(&request.body.issuer, &request.body.invitation_id);
        let published =
            tokio::time::timeout(QUERY_DEADLINE, self.transport.put_record(key, value)).await;
        if !matches!(published, Ok(Ok(()))) {
            self.set_status(
                "failed",
                "Could not publish enrollment. Check the network connection.",
            );
            bail!("could not publish enrollment request");
        }
        self.set_status(
            "pending",
            "Waiting for the invitation issuer to approve this device.",
        );
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let _permit = permit;
            let started = tokio::time::Instant::now();
            let key = approval_key(
                &request.body.issuer,
                &request.body.invitation_id,
                &request.body.requester,
            );
            loop {
                let Some(control) = weak.upgrade() else { break };
                let now = unix_seconds();
                if started.elapsed() >= ENROLLMENT_DEADLINE || now >= request.body.expires_at {
                    control.set_status(
                        "failed",
                        "Enrollment timed out. Request a new invitation or retry.",
                    );
                    break;
                }
                let records =
                    tokio::time::timeout(QUERY_DEADLINE, control.transport.get_record(key.clone()))
                        .await;
                if let Ok(Ok(records)) = records {
                    for bytes in records.into_iter().take(MAX_RECORDS) {
                        if bytes.len() > MAX_RECORD_BYTES {
                            continue;
                        }
                        let Ok(approval) = serde_json::from_slice::<Approval>(&bytes) else {
                            continue;
                        };
                        if validate_approval(&approval, &request, &invitation.descriptor, now)
                            .is_err()
                        {
                            continue;
                        }
                        if control.store.accept_approval(approval).is_ok() {
                            control.set_status("authorized", "Enrollment approved. Available models can now accept your requests.");
                        } else {
                            control.set_status(
                                "failed",
                                "Could not save enrollment. Check private application storage.",
                            );
                        }
                        return;
                    }
                }
                drop(control);
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        });
        Ok(self.enrollment_status())
    }

    /// Poll only bounded, operator-created invitation mailboxes. A weak
    /// reference prevents the task from keeping a stopped control alive.
    pub fn spawn_server(self: &Arc<Self>) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(control) = weak.upgrade() else { break };
                control.poll_issued_invitations().await;
                drop(control);
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
    }

    async fn poll_issued_invitations(&self) {
        let invites = self.store.pending_invitations();
        stream::iter(invites)
            .for_each_concurrent(4, |descriptor| async move {
                let key = request_key(&descriptor.issuer, &descriptor.id);
                let records =
                    tokio::time::timeout(QUERY_DEADLINE, self.transport.get_record(key)).await;
                let Ok(Ok(records)) = records else { return };
                for bytes in records.into_iter().take(MAX_RECORDS) {
                    if bytes.len() > MAX_RECORD_BYTES {
                        continue;
                    }
                    let Ok(request) = serde_json::from_slice::<EnrollmentRequest>(&bytes) else {
                        continue;
                    };
                    let Ok(body) = self.store.claim(&request, unix_seconds()) else {
                        continue;
                    };
                    let Ok(approval) = (|| -> Result<Approval> {
                        Ok(Approval {
                            issuer_signature: sign(&self.identity, APPROVAL_DOMAIN, &body)?,
                            body,
                        })
                    })() else {
                        continue;
                    };
                    let Ok(value) = serde_json::to_vec(&approval) else {
                        continue;
                    };
                    let key = approval_key(
                        &request.body.issuer,
                        &request.body.invitation_id,
                        &request.body.requester,
                    );
                    let _ =
                        tokio::time::timeout(QUERY_DEADLINE, self.transport.put_record(key, value))
                            .await;
                }
            })
            .await;
    }
}

fn validate_descriptor(descriptor: &InvitationDescriptor, now: u64) -> Result<()> {
    if descriptor.schema != SCHEMA
        || descriptor.id.len() != 32
        || !descriptor
            .id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("invalid invitation descriptor");
    }
    decode_key(&descriptor.issuer)?;
    decode_key(&descriptor.delegation_key)?;
    if descriptor.expires_at <= now
        || descriptor.expires_at > now.saturating_add(MAX_INVITE_TTL_SECS)
        || descriptor.grant_expires_at < descriptor.expires_at
        || descriptor.grant_expires_at > now.saturating_add(GRANT_TTL_SECS)
    {
        bail!("invitation is expired or outside permitted time bounds");
    }
    Ok(())
}

fn parse_invitation(encoded: &str, now: u64) -> Result<(Invitation, SigningKey)> {
    if encoded.len() > MAX_INVITATION_BYTES {
        bail!("invitation exceeds size limit")
    }
    let text = encoded.trim();
    let bytes = if let Some(value) = text.strip_prefix("lucid://join/") {
        URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| anyhow!("invalid invitation encoding"))?
    } else {
        text.as_bytes().to_vec()
    };
    let invitation: Invitation =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid invitation document"))?;
    validate_descriptor(&invitation.descriptor, now)?;
    verify(
        &invitation.descriptor.issuer,
        INVITE_DOMAIN,
        &invitation.descriptor,
        &invitation.issuer_signature,
    )?;
    let delegation = SigningKey::from_bytes(&decode_key(&invitation.delegation_seed)?);
    if hex(&delegation.verifying_key().to_bytes()) != invitation.descriptor.delegation_key {
        bail!("invitation credential does not match its delegation key");
    }
    Ok((invitation, delegation))
}

fn validate_request(
    request: &EnrollmentRequest,
    descriptor: &InvitationDescriptor,
    now: u64,
) -> Result<()> {
    validate_descriptor(descriptor, now)?;
    if request.body.schema != SCHEMA
        || request.body.issuer != descriptor.issuer
        || request.body.invitation_id != descriptor.id
        || request.body.expires_at != descriptor.expires_at
    {
        bail!("enrollment request does not match invitation");
    }
    decode_key(&request.body.requester)?;
    verify(
        &descriptor.delegation_key,
        REQUEST_DOMAIN,
        &request.body,
        &request.delegation_signature,
    )?;
    verify(
        &request.body.requester,
        REQUEST_DOMAIN,
        &request.body,
        &request.requester_signature,
    )?;
    Ok(())
}

fn validate_approval(
    approval: &Approval,
    request: &EnrollmentRequest,
    descriptor: &InvitationDescriptor,
    now: u64,
) -> Result<()> {
    let body = &approval.body;
    if body.schema != SCHEMA
        || body.issuer != descriptor.issuer
        || body.invitation_id != descriptor.id
        || body.requester != request.body.requester
        || body.request_hash != request_hash(request)?
        || body.expires_at != descriptor.grant_expires_at
        || body.expires_at <= now
    {
        bail!("approval does not match enrollment request");
    }
    verify(
        &body.issuer,
        APPROVAL_DOMAIN,
        body,
        &approval.issuer_signature,
    )
}

fn validate_ledger(ledger: &Ledger) -> Result<()> {
    if ledger.issued.len() > MAX_PENDING_INVITES
        || ledger.grants.len() > MAX_GRANTS
        || ledger.received.len() > MAX_GRANTS
    {
        bail!("enrollment ledger count limit exceeded");
    }
    decode_key(&ledger.owner)?;
    for issued in &ledger.issued {
        if issued.descriptor.issuer != ledger.owner {
            bail!("foreign invitation in local ledger")
        }
        decode_key(&issued.descriptor.delegation_key)?;
        if let Some(requester) = &issued.claimed_requester {
            decode_key(requester)?;
        }
    }
    for grant in &ledger.grants {
        if grant.schema != SCHEMA
            || grant.issuer != ledger.owner
            || grant.expires_at > unix_seconds().saturating_add(GRANT_TTL_SECS)
        {
            bail!("invalid enrollment grant");
        }
        decode_hex(&grant.request_hash, 32)?;
        decode_key(&grant.requester)?;
    }
    for approval in &ledger.received {
        if approval.body.schema != SCHEMA
            || approval.body.requester != ledger.owner
            || approval.body.expires_at > unix_seconds().saturating_add(GRANT_TTL_SECS)
        {
            bail!("invalid received enrollment grant");
        }
        verify(
            &approval.body.issuer,
            APPROVAL_DOMAIN,
            &approval.body,
            &approval.issuer_signature,
        )?;
    }
    Ok(())
}

fn request_key(issuer: &str, id: &str) -> Vec<u8> {
    format!("phase/lucid-enrollment/request/v1/{issuer}/{id}").into_bytes()
}
fn approval_key(issuer: &str, id: &str, requester: &str) -> Vec<u8> {
    format!("phase/lucid-enrollment/approval/v1/{issuer}/{id}/{requester}").into_bytes()
}
fn request_hash(request: &EnrollmentRequest) -> Result<String> {
    Ok(hex(&Sha256::digest(domain_bytes(REQUEST_DOMAIN, request)?)))
}
fn domain_bytes<T: Serialize>(domain: &str, value: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&(domain, value))?)
}
fn sign<T: Serialize>(identity: &NodeIdentity, domain: &str, value: &T) -> Result<String> {
    Ok(hex(&identity
        .sign(&domain_bytes(domain, value)?)
        .to_bytes()))
}
fn verify<T: Serialize>(public_key: &str, domain: &str, value: &T, signature: &str) -> Result<()> {
    let key = VerifyingKey::from_bytes(&decode_key(public_key)?)
        .map_err(|_| anyhow!("invalid enrollment public key"))?;
    let signature = Signature::from_slice(&decode_hex(signature, 64)?)
        .map_err(|_| anyhow!("invalid enrollment signature"))?;
    key.verify_strict(&domain_bytes(domain, value)?, &signature)
        .map_err(|_| anyhow!("enrollment signature verification failed"))
}
fn decode_key(value: &str) -> Result<[u8; 32]> {
    decode_hex(value, 32)?
        .try_into()
        .map_err(|_| anyhow!("invalid enrollment key"))
}
fn decode_hex(value: &str, bytes: usize) -> Result<Vec<u8>> {
    if value.len() != bytes * 2
        || !value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("invalid enrollment hexadecimal value");
    }
    (0..bytes)
        .map(|i| {
            u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
                .map_err(|_| anyhow!("invalid enrollment hexadecimal value"))
        })
        .collect()
}
fn hex(value: &[u8]) -> String {
    value.iter().map(|b| format!("{b:02x}")).collect()
}
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn ensure_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(parent)
            .context("create private control directory")?;
    }
    if !fs::symlink_metadata(parent)?.file_type().is_dir() {
        bail!("control directory must not be a symlink")
    }
    Ok(())
}
fn read_private(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > limit as u64 {
        bail!("invalid private control file")
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("private control file permissions must be 0600")
        }
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("private control file exceeds size limit")
    }
    Ok(Some(bytes))
}
fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure_parent(path)?;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_file() {
            bail!("private control path must be a regular file")
        }
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(
        ".lucid-control-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn control_token(path: &Path) -> Result<String> {
    ensure_parent(path)?;
    if let Some(bytes) = read_private(path, 64)? {
        let token = String::from_utf8(bytes).context("invalid control token encoding")?;
        decode_key(&token)?;
        return Ok(token);
    }
    // A separate fresh OS-random seed; never reuse the node identity as a bearer.
    let token = hex(&NodeIdentity::generate().signing_key().to_bytes());
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(
        ".lucid-token-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    // Publish a complete token exclusively, like phase-identity's identity
    // storage. A racing reader never observes an empty/partially written file.
    let published = fs::hard_link(&temporary, path);
    let _ = fs::remove_file(&temporary);
    match published {
        Ok(()) => {
            #[cfg(unix)]
            fs::File::open(parent)?.sync_all()?;
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let bytes =
                read_private(path, 64)?.ok_or_else(|| anyhow!("control token unavailable"))?;
            let token = String::from_utf8(bytes).context("invalid control token encoding")?;
            decode_key(&token)?;
            Ok(token)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyConfig, PolicyState};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[derive(Default)]
    struct MemoryDht(Mutex<HashMap<Vec<u8>, Vec<u8>>>);
    #[async_trait]
    impl DhtTransport for MemoryDht {
        async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
            self.0.lock().unwrap().insert(key, value);
            Ok(())
        }
        async fn get_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .into_iter()
                .collect())
        }
    }
    fn setup(
        identity: NodeIdentity,
        dht: Arc<MemoryDht>,
        dir: &Path,
    ) -> (Arc<ConsumerControl>, Arc<PolicyEngine>) {
        let policy = Arc::new(PolicyEngine::new_for_tests(
            PolicyConfig::default(),
            PolicyState::default(),
        ));
        let control = ConsumerControl::open(
            identity,
            dht,
            Arc::clone(&policy),
            dir.join("enrollment.json"),
            dir.join("control.token"),
        )
        .unwrap();
        (control, policy)
    }
    fn request_from(
        invitation: &str,
        identity: &NodeIdentity,
    ) -> (EnrollmentRequest, InvitationDescriptor) {
        let (invitation, delegation) = parse_invitation(invitation, unix_seconds()).unwrap();
        let body = RequestBody {
            schema: SCHEMA,
            issuer: invitation.descriptor.issuer.clone(),
            invitation_id: invitation.descriptor.id.clone(),
            requester: hex(&identity.peer_id_bytes()),
            expires_at: invitation.descriptor.expires_at,
        };
        let bytes = domain_bytes(REQUEST_DOMAIN, &body).unwrap();
        (
            EnrollmentRequest {
                body,
                delegation_signature: hex(&delegation.sign(&bytes).to_bytes()),
                requester_signature: hex(&identity.sign(&bytes).to_bytes()),
            },
            invitation.descriptor,
        )
    }

    #[tokio::test]
    async fn invitation_is_one_use_idempotent_for_same_request_and_durable() {
        let tmp = TempDir::new().unwrap();
        let dht = Arc::new(MemoryDht::default());
        let issuer = NodeIdentity::generate();
        let (control, policy) = setup(issuer.clone(), Arc::clone(&dht), tmp.path());
        let invitation = control.create_invitation(900).await.unwrap();
        let client = NodeIdentity::generate();
        let (request, descriptor) = request_from(&invitation.invitation, &client);
        assert!(descriptor.grant_expires_at > descriptor.expires_at);
        assert!(!policy.is_authorized_submitter(&request.body.requester));
        let grant = control.store.claim(&request, unix_seconds()).unwrap();
        assert!(policy.is_authorized_submitter(&request.body.requester));
        let client_peer = peer_id_from_ed25519_pubkey(&client.peer_id_bytes()).unwrap();
        assert_eq!(
            control.authorized_circle_peers(&policy),
            HashSet::from([client_peer])
        );
        assert_eq!(
            control
                .store
                .claim(&request, unix_seconds())
                .unwrap()
                .request_hash,
            grant.request_hash
        );
        let other = NodeIdentity::generate();
        let (other_request, _) = request_from(&invitation.invitation, &other);
        assert!(control.store.claim(&other_request, unix_seconds()).is_err());
        assert!(!policy.is_authorized_submitter(&other_request.body.requester));
        drop(control);
        drop(policy);
        let (_, reopened_policy) = setup(issuer, dht, tmp.path());
        assert!(reopened_policy.is_authorized_submitter(&request.body.requester));
    }

    #[tokio::test]
    async fn forged_and_expired_enrollment_requests_never_authorize() {
        let tmp = TempDir::new().unwrap();
        let (control, policy) = setup(
            NodeIdentity::generate(),
            Arc::new(MemoryDht::default()),
            tmp.path(),
        );
        let invitation = control.create_invitation(60).await.unwrap();
        let client = NodeIdentity::generate();
        let (request, descriptor) = request_from(&invitation.invitation, &client);
        let mut forged = request.clone();
        forged.body.requester = hex(&NodeIdentity::generate().peer_id_bytes());
        assert!(control.store.claim(&forged, unix_seconds()).is_err());
        let mut forged = request.clone();
        forged.requester_signature = "00".repeat(64);
        assert!(control.store.claim(&forged, unix_seconds()).is_err());
        let mut forged = request.clone();
        forged.delegation_signature = "00".repeat(64);
        assert!(control.store.claim(&forged, unix_seconds()).is_err());
        assert!(control
            .store
            .claim(&request, descriptor.expires_at)
            .is_err());
        assert!(parse_invitation(&invitation.invitation, descriptor.expires_at).is_err());
        assert!(!policy.is_authorized_submitter(&request.body.requester));
        assert!(control.create_invitation(0).await.is_err());
        assert!(control
            .create_invitation(MAX_INVITE_TTL_SECS + 1)
            .await
            .is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn enrollment_completes_over_public_records_without_publishing_seed_and_survives_client_restart(
    ) {
        let tmp = TempDir::new().unwrap();
        let dht = Arc::new(MemoryDht::default());
        let issuer = NodeIdentity::generate();
        let client = NodeIdentity::generate();
        let (server, policy) = setup(issuer.clone(), Arc::clone(&dht), &tmp.path().join("server"));
        let (consumer, _) = setup(client.clone(), Arc::clone(&dht), &tmp.path().join("client"));
        let response = server.create_invitation(900).await.unwrap();
        let (private_invitation, _) =
            parse_invitation(&response.invitation, unix_seconds()).unwrap();
        assert_eq!(
            consumer.enroll(&response.invitation).await.unwrap().state,
            "pending"
        );
        server.poll_issued_invitations().await;
        for _ in 0..10 {
            if consumer.enrollment_status().state == "authorized" {
                break;
            }
            tokio::time::advance(Duration::from_secs(2)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(consumer.enrollment_status().state, "authorized");
        assert!(consumer.is_enrolled_with(&issuer.peer_id_bytes()));
        assert!(policy.is_authorized_submitter(&hex(&client.peer_id_bytes())));
        for bytes in dht.0.lock().unwrap().values() {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains(&private_invitation.delegation_seed));
            assert!(!text.contains("delegation_seed"));
        }
        let ledger = fs::read_to_string(tmp.path().join("server/enrollment.json")).unwrap();
        assert!(!ledger.contains(&private_invitation.delegation_seed));
        assert!(!ledger.contains("delegation_seed"));
        drop(consumer);
        let (reopened, reopened_policy) =
            setup(client, Arc::clone(&dht), &tmp.path().join("client"));
        assert_eq!(reopened.enrollment_status().state, "authorized");
        assert_eq!(reopened.authorized_issuers(), vec![issuer.peer_id_bytes()]);
        assert_eq!(
            reopened.authorized_issuer_peers().collect::<Vec<_>>(),
            vec![peer_id_from_ed25519_pubkey(&issuer.peer_id_bytes()).unwrap()]
        );
        assert_eq!(
            reopened.authorized_circle_peers(&reopened_policy),
            HashSet::from([peer_id_from_ed25519_pubkey(&issuer.peer_id_bytes()).unwrap()])
        );
    }

    #[tokio::test]
    async fn expired_grants_fail_closed_after_restart() {
        let tmp = TempDir::new().unwrap();
        let dht = Arc::new(MemoryDht::default());
        let issuer = NodeIdentity::generate();
        let (control, policy) = setup(issuer.clone(), Arc::clone(&dht), tmp.path());
        let invitation = control.create_invitation(60).await.unwrap();
        let client = NodeIdentity::generate();
        let (request, _) = request_from(&invitation.invitation, &client);
        control.store.claim(&request, unix_seconds()).unwrap();
        {
            let mut ledger = control.store.ledger.lock().unwrap();
            ledger.grants[0].expires_at = unix_seconds() - 1;
            control.store.persist(&ledger).unwrap();
        }
        assert!(!policy.is_authorized_submitter(&request.body.requester));
        assert!(control.authorized_circle_peers(&policy).is_empty());
        let (control, reopened) = setup(issuer, dht, tmp.path());
        assert!(!reopened.is_authorized_submitter(&request.body.requester));
        assert!(control.authorized_circle_peers(&reopened).is_empty());
    }

    #[tokio::test]
    async fn failed_atomic_persistence_does_not_consume_or_authorize_invitation() {
        let tmp = TempDir::new().unwrap();
        let (control, policy) = setup(
            NodeIdentity::generate(),
            Arc::new(MemoryDht::default()),
            tmp.path(),
        );
        let invitation = control.create_invitation(60).await.unwrap();
        let client = NodeIdentity::generate();
        let (request, _) = request_from(&invitation.invitation, &client);
        let path = &control.store.path;
        let backup = tmp.path().join("saved-ledger.json");
        fs::rename(path, &backup).unwrap();
        fs::create_dir(path).unwrap();
        assert!(control.store.claim(&request, unix_seconds()).is_err());
        assert!(!policy.is_authorized_submitter(&request.body.requester));
        fs::remove_dir(path).unwrap();
        fs::rename(backup, path).unwrap();
        assert!(control.store.claim(&request, unix_seconds()).is_ok());
        assert!(policy.is_authorized_submitter(&request.body.requester));
    }

    #[tokio::test]
    async fn control_token_auth_and_input_bounds_are_enforced() {
        let tmp = TempDir::new().unwrap();
        let identity = NodeIdentity::generate();
        let dht = Arc::new(MemoryDht::default());
        let (control, _) = setup(identity.clone(), Arc::clone(&dht), tmp.path());
        let token = fs::read_to_string(tmp.path().join("control.token")).unwrap();
        assert_eq!(token.len(), 64);
        assert!(control.authorize_bearer(&format!("Bearer {token}")));
        assert!(!control.authorize_bearer(&token));
        assert!(!control.authorize_bearer("Bearer wrong"));
        assert!(control
            .enroll(&"x".repeat(MAX_INVITATION_BYTES + 1))
            .await
            .is_err());
        let (reopened, _) = setup(identity, dht, tmp.path());
        assert!(reopened.authorize_bearer(&format!("Bearer {token}")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(tmp.path().join("control.token"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            fs::set_permissions(
                tmp.path().join("control.token"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert!(ConsumerControl::open(
                NodeIdentity::generate(),
                Arc::new(MemoryDht::default()),
                Arc::new(PolicyEngine::new_for_tests(
                    PolicyConfig::default(),
                    PolicyState::default()
                )),
                tmp.path().join("other.json"),
                tmp.path().join("control.token")
            )
            .is_err());
        }
    }

    #[tokio::test]
    async fn invitation_count_is_bounded() {
        let tmp = TempDir::new().unwrap();
        let (control, _) = setup(
            NodeIdentity::generate(),
            Arc::new(MemoryDht::default()),
            tmp.path(),
        );
        for _ in 0..MAX_PENDING_INVITES {
            control.create_invitation(60).await.unwrap();
        }
        assert!(control.create_invitation(60).await.is_err());
    }

    #[tokio::test]
    async fn forged_approval_and_cross_request_approval_are_rejected() {
        let tmp = TempDir::new().unwrap();
        let issuer = NodeIdentity::generate();
        let (control, _) = setup(issuer.clone(), Arc::new(MemoryDht::default()), tmp.path());
        let invitation = control.create_invitation(60).await.unwrap();
        let client = NodeIdentity::generate();
        let (request, descriptor) = request_from(&invitation.invitation, &client);
        let body = control.store.claim(&request, unix_seconds()).unwrap();
        let approval = Approval {
            issuer_signature: sign(&issuer, APPROVAL_DOMAIN, &body).unwrap(),
            body,
        };
        assert!(validate_approval(&approval, &request, &descriptor, unix_seconds()).is_ok());
        let mut forged = approval.clone();
        forged.issuer_signature = "00".repeat(64);
        assert!(validate_approval(&forged, &request, &descriptor, unix_seconds()).is_err());
        let mut substituted = request.clone();
        substituted.body.requester = hex(&NodeIdentity::generate().peer_id_bytes());
        assert!(validate_approval(&approval, &substituted, &descriptor, unix_seconds()).is_err());
        let mut extended = approval.clone();
        extended.body.expires_at += 1;
        extended.issuer_signature = sign(&issuer, APPROVAL_DOMAIN, &extended.body).unwrap();
        assert!(validate_approval(&extended, &request, &descriptor, unix_seconds()).is_err());
    }

    #[test]
    fn concurrent_control_token_creators_publish_one_complete_private_token() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("token");
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    control_token(&path).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let values = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>();
        assert!(values.iter().all(|t| t == &values[0] && t.len() == 64));
        assert_eq!(fs::read_to_string(path).unwrap(), values[0]);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn control_files_reject_symlink_destinations() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        fs::write(&real, "f".repeat(64)).unwrap();
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();
        assert!(control_token(&link).is_err());
        assert!(atomic_private_write(&link, b"private").is_err());
        assert_eq!(fs::read_to_string(real).unwrap(), "f".repeat(64));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_initial_publish_releases_enrollment_and_retry_completes() {
        #[derive(Default)]
        struct CancelFirstPublish {
            data: MemoryDht,
            attempts: std::sync::atomic::AtomicUsize,
            entered: tokio::sync::Notify,
        }
        #[async_trait]
        impl DhtTransport for CancelFirstPublish {
            async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
                if self
                    .attempts
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                    == 0
                {
                    self.entered.notify_one();
                    std::future::pending::<()>().await;
                }
                self.data.put_record(key, value).await
            }
            async fn get_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>> {
                self.data.get_record(key).await
            }
        }
        let tmp = TempDir::new().unwrap();
        let dht = Arc::new(CancelFirstPublish::default());
        let issuer = NodeIdentity::generate();
        let client = NodeIdentity::generate();
        let make_control = |identity: NodeIdentity, dir: &Path| {
            let policy = Arc::new(PolicyEngine::new_for_tests(
                PolicyConfig::default(),
                PolicyState::default(),
            ));
            ConsumerControl::open(
                identity,
                dht.clone(),
                policy,
                dir.join("enrollment.json"),
                dir.join("control.token"),
            )
            .unwrap()
        };
        let server = make_control(issuer.clone(), &tmp.path().join("server"));
        let consumer = make_control(client, &tmp.path().join("consumer"));
        let invitation = server.create_invitation(900).await.unwrap().invitation;
        let cancelled_consumer = Arc::clone(&consumer);
        let cancelled_invitation = invitation.clone();
        let cancelled =
            tokio::spawn(async move { cancelled_consumer.enroll(&cancelled_invitation).await });
        dht.entered.notified().await;
        assert!(consumer
            .enrolling
            .load(std::sync::atomic::Ordering::Acquire));
        cancelled.abort();
        assert!(cancelled.await.unwrap_err().is_cancelled());
        assert!(!consumer
            .enrolling
            .load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(consumer.enroll(&invitation).await.unwrap().state, "pending");
        // The permit must remain owned by the background task after returning
        // the pending HTTP response, so a second live enrollment still refuses.
        assert!(consumer.enroll(&invitation).await.is_err());
        server.poll_issued_invitations().await;
        for _ in 0..10 {
            if consumer.enrollment_status().state == "authorized" {
                break;
            }
            tokio::time::advance(Duration::from_secs(2)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(consumer.enrollment_status().state, "authorized");
        assert!(consumer.is_enrolled_with(&issuer.peer_id_bytes()));
        assert!(!consumer
            .enrolling
            .load(std::sync::atomic::Ordering::Acquire));
    }
}
