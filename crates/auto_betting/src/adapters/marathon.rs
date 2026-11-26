use async_trait::async_trait;
use chrono::Utc;
use shared::{
    BetExecutionReceipt, BetExecutionRequest, BetExecutionStatus, BookmakerAccount,
    BookmakerAccountCapabilityMetadata, BookmakerAdapterAuthMetadata,
    BookmakerAdapterReadinessMetadata, BookmakerAdapterReadinessStage, BookmakerBalanceRefresh,
    BookmakerBalanceRefreshState, BookmakerBalanceSnapshot, BookmakerExecutionCapability,
    BookmakerExecutionMode, BookmakerSession, BookmakerSessionState, BookmakerSessionStatus,
    BookmakerSessionSyncState,
};

use crate::auth::{AuthError, AuthSession, BookmakerSessionMaterial};
use crate::execution::BookmakerExecutionAdapter;

#[derive(Debug, Clone, Default)]
pub struct MarathonExecutionAdapter;

impl MarathonExecutionAdapter {
    pub const BOOKMAKER: &'static str = "marathon";
    const API_BASE_URL: &'static str = "https://api.marathon.mock";

    fn dry_run_message(request: &BetExecutionRequest) -> String {
        format!(
            "marathon execution stub: dry-run only for {} {} on event {}",
            request.market, request.selection, request.event_id
        )
    }

    pub fn api_base_url(&self) -> &'static str {
        Self::API_BASE_URL
    }

    pub fn planned_endpoints(&self) -> [&'static str; 3] {
        ["session", "balance", "coupon/place"]
    }

    fn session_status(
        &self,
        account: &BookmakerAccount,
        session: Option<&BookmakerSession>,
        session_material: Option<&BookmakerSessionMaterial>,
    ) -> BookmakerSessionStatus {
        let checked_at = Utc::now();

        match session {
            None => BookmakerSessionStatus {
                account_id: Some(account.id),
                bookmaker: Self::BOOKMAKER.into(),
                sync_state: BookmakerSessionSyncState::NoSession,
                authenticated: false,
                can_refresh_balance: false,
                detail: Some("marathon session is not configured".into()),
                checked_at,
            },
            Some(session) => {
                let has_imported_material = session_material
                    .map(BookmakerSessionMaterial::has_usable_cookie_header)
                    .unwrap_or(false);
                let (sync_state, authenticated, can_refresh_balance, detail) = match session.state {
                    BookmakerSessionState::Configured => (
                        BookmakerSessionSyncState::Configured,
                        false,
                        false,
                        "marathon session configured but not authenticated",
                    ),
                    BookmakerSessionState::Active => (
                        BookmakerSessionSyncState::Configured,
                        false,
                        false,
                        if has_imported_material {
                            "marathon cookie material is present but cannot be validated; authentication and live balance APIs are not implemented"
                        } else {
                            "marathon session is marked active but the stub adapter cannot verify it"
                        },
                    ),
                    BookmakerSessionState::Expired => (
                        BookmakerSessionSyncState::Expired,
                        false,
                        false,
                        "marathon session expired",
                    ),
                    BookmakerSessionState::Locked => (
                        BookmakerSessionSyncState::Locked,
                        false,
                        false,
                        "marathon session locked",
                    ),
                    BookmakerSessionState::Disconnected => (
                        BookmakerSessionSyncState::Disconnected,
                        false,
                        false,
                        "marathon session disconnected",
                    ),
                };

                BookmakerSessionStatus {
                    account_id: Some(account.id),
                    bookmaker: Self::BOOKMAKER.into(),
                    sync_state,
                    authenticated,
                    can_refresh_balance,
                    detail: Some(detail.into()),
                    checked_at,
                }
            }
        }
    }
}

#[async_trait]
impl BookmakerExecutionAdapter for MarathonExecutionAdapter {
    fn capability(&self) -> BookmakerExecutionCapability {
        BookmakerExecutionCapability {
            bookmaker: Self::BOOKMAKER.into(),
            supports_dry_run: true,
            supports_balance_snapshot: true,
            supports_bet_placement: false,
            supports_real_money: false,
            requires_session: true,
            account_metadata: BookmakerAccountCapabilityMetadata {
                api_base_url: Some(self.api_base_url().into()),
                planned_endpoints: self
                    .planned_endpoints()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                supports_read_only_session_sync: true,
                supports_read_only_balance_refresh: true,
                remote_balance_fetch_enabled: false,
                auth: BookmakerAdapterAuthMetadata {
                    flow: "manual_cookie_session".into(),
                    requires_human_bootstrap: true,
                    session_bootstrap_enabled: false,
                    session_refresh_enabled: false,
                    persisted_snapshot_enabled: true,
                },
                readiness: BookmakerAdapterReadinessMetadata {
                    stage: BookmakerAdapterReadinessStage::Stub,
                    safe_mode_only: true,
                    approval_reference_required: false,
                    operator_notes: vec![
                        "Marathon adapter is a stub; authentication and live API calls are not implemented".into(),
                        "only dry-run and cached balance snapshots are available; bet placement is disabled".into(),
                    ],
                },
                notes: vec![
                    "stub adapter exposes dry-run and cached account state only".into(),
                    "authentication, live balance HTTP calls and coupon placement are not implemented".into(),
                ],
            },
        }
    }

    async fn dry_run(
        &self,
        account: Option<&BookmakerAccount>,
        request: &BetExecutionRequest,
    ) -> Result<BetExecutionReceipt, String> {
        Ok(BetExecutionReceipt {
            ticket_id: None,
            account_id: account.map(|account| account.id),
            bookmaker: Self::BOOKMAKER.into(),
            status: BetExecutionStatus::DryRun,
            mode: BookmakerExecutionMode::DryRun,
            accepted_stake: request.stake,
            accepted_odds: request.odds,
            message: Some(Self::dry_run_message(request)),
            placed_at: Utc::now(),
        })
    }

    async fn get_session_status(
        &self,
        account: &BookmakerAccount,
        session: Option<&BookmakerSession>,
        session_material: Option<&BookmakerSessionMaterial>,
    ) -> Result<BookmakerSessionStatus, String> {
        Ok(self.session_status(account, session, session_material))
    }

    async fn refresh_balance_snapshot(
        &self,
        account: &BookmakerAccount,
        session_status: &BookmakerSessionStatus,
        cached_snapshot: Option<&BookmakerBalanceSnapshot>,
        session_material: Option<&BookmakerSessionMaterial>,
    ) -> Result<BookmakerBalanceRefresh, String> {
        let checked_at = Utc::now();
        let snapshot = cached_snapshot.cloned();
        let state = match (&session_status.sync_state, snapshot.is_some()) {
            (BookmakerSessionSyncState::NoSession, _) => BookmakerBalanceRefreshState::NoSession,
            (BookmakerSessionSyncState::Configured, _) => {
                BookmakerBalanceRefreshState::SessionNotAuthenticated
            }
            (BookmakerSessionSyncState::Expired, _) => {
                BookmakerBalanceRefreshState::SessionNotAuthenticated
            }
            (BookmakerSessionSyncState::Locked, _) => {
                BookmakerBalanceRefreshState::SessionNotAuthenticated
            }
            (BookmakerSessionSyncState::Disconnected, _) => {
                BookmakerBalanceRefreshState::SessionNotAuthenticated
            }
            (BookmakerSessionSyncState::Authenticated, true) => {
                BookmakerBalanceRefreshState::CachedBalanceAvailable
            }
            (BookmakerSessionSyncState::Authenticated, false) => {
                BookmakerBalanceRefreshState::AuthenticatedBalanceUnavailable
            }
        };

        let has_imported_material = session_material
            .map(BookmakerSessionMaterial::has_credentials)
            .unwrap_or(false);
        let detail = match state {
            BookmakerBalanceRefreshState::NoSession => {
                "marathon balance refresh skipped: no session configured"
            }
            BookmakerBalanceRefreshState::SessionNotAuthenticated => {
                "marathon balance refresh skipped: session is not authenticated"
            }
            BookmakerBalanceRefreshState::AuthenticatedBalanceUnavailable => {
                if has_imported_material {
                    "marathon real session material imported; remote balance endpoint is not wired yet"
                } else {
                    "marathon session is authenticated but remote balance fetch is disabled in safe mode"
                }
            }
            BookmakerBalanceRefreshState::CachedBalanceAvailable => {
                if has_imported_material {
                    "marathon returned cached balance snapshot for imported browser session"
                } else {
                    "marathon returned cached balance snapshot; remote refresh remains disabled"
                }
            }
            BookmakerBalanceRefreshState::RemoteBalanceFetched => {
                "marathon balance fetched from live API"
            }
        };

        Ok(BookmakerBalanceRefresh {
            account_id: Some(account.id),
            bookmaker: Self::BOOKMAKER.into(),
            state,
            session_status: session_status.clone(),
            snapshot,
            detail: Some(detail.into()),
            checked_at,
        })
    }

    async fn place_bet(
        &self,
        account: &BookmakerAccount,
        request: &BetExecutionRequest,
    ) -> Result<BetExecutionReceipt, String> {
        let _ = request;
        Ok(BetExecutionReceipt {
            ticket_id: None,
            account_id: Some(account.id),
            bookmaker: Self::BOOKMAKER.into(),
            status: BetExecutionStatus::Blocked,
            mode: account.mode.clone(),
            accepted_stake: 0.0,
            accepted_odds: 0.0,
            message: Some(
                "marathon bet placement is not implemented; no request was sent and no ticket was created".into(),
            ),
            placed_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::BookmakerExecutionMode;
    use uuid::Uuid;

    fn account() -> BookmakerAccount {
        BookmakerAccount {
            id: Uuid::new_v4(),
            bookmaker: MarathonExecutionAdapter::BOOKMAKER.into(),
            label: "main".into(),
            currency: "RUB".into(),
            enabled: true,
            mode: BookmakerExecutionMode::DryRun,
            created_at: Utc::now(),
            last_used_at: None,
        }
    }

    #[test]
    fn capability_does_not_advertise_unimplemented_placement() {
        let capability = MarathonExecutionAdapter.capability();
        assert!(!capability.supports_bet_placement);
        assert!(!capability.supports_real_money);
        assert_eq!(capability.account_metadata.readiness.stage, BookmakerAdapterReadinessStage::Stub);
    }

    #[test]
    fn active_session_is_not_authenticated_by_the_stub_adapter() {
        let adapter = MarathonExecutionAdapter;
        let account = account();
        let session = BookmakerSession {
            account_id: account.id,
            bookmaker: account.bookmaker.clone(),
            state: BookmakerSessionState::Active,
            token_hint: Some("old...".into()),
            last_synced_at: Utc::now(),
            expires_at: None,
        };
        let status = futures::executor::block_on(adapter.get_session_status(&account, Some(&session), None))
            .expect("status should be available");

        assert_eq!(status.sync_state, BookmakerSessionSyncState::Configured);
        assert!(!status.authenticated);
        assert!(!status.can_refresh_balance);
    }

    #[test]
    fn place_bet_fails_closed_without_a_coupon_endpoint() {
        let adapter = MarathonExecutionAdapter;
        let account = account();
        let request = BetExecutionRequest {
            bookmaker: MarathonExecutionAdapter::BOOKMAKER.into(),
            event_id: "event-1".into(),
            market: "1X2".into(),
            selection: "1".into(),
            odds: 2.1,
            stake: 500.0,
            allow_dry_run: false,
            reference: Some("operator-approval".into()),
        };
        let receipt = futures::executor::block_on(adapter.place_bet(&account, &request))
            .expect("unsupported placement should return a blocked receipt");
        assert_eq!(receipt.status, BetExecutionStatus::Blocked);
        assert!(receipt.ticket_id.is_none());
        assert_eq!(receipt.accepted_stake, 0.0);
        assert!(receipt.message.as_deref().unwrap_or_default().contains("no request was sent"));
    }

    #[test]
    fn authorize_fails_closed_until_authentication_is_implemented() {
        let adapter = MarathonExecutionAdapter;
        let account = account();
        let error = futures::executor::block_on(adapter.authorize(&account))
            .expect_err("stub authentication must not create a fake active session");
        assert!(error.to_string().contains("authentication is not implemented"));
    }
}

use crate::auth::BookmakerAuth;

#[async_trait]
impl BookmakerAuth for MarathonExecutionAdapter {
    async fn authorize(&self, account: &BookmakerAccount) -> Result<AuthSession, AuthError> {
        Err(AuthError::AuthFailed {
            bookmaker: account.bookmaker.clone(),
            error: "Marathon authentication is not implemented".into(),
        })
    }

    async fn check_session(&self, _session: &AuthSession) -> Result<bool, AuthError> {
        Ok(false)
    }
}
