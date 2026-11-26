use async_trait::async_trait;
use chrono::{Duration, Utc};
use shared::{
    BetExecutionReceipt, BetExecutionRequest, BetExecutionStatus, BookmakerAccount,
    BookmakerAccountCapabilityMetadata, BookmakerAdapterAuthMetadata,
    BookmakerAdapterReadinessMetadata, BookmakerAdapterReadinessStage, BookmakerBalanceRefresh,
    BookmakerBalanceRefreshState, BookmakerBalanceSnapshot, BookmakerExecutionCapability,
    BookmakerExecutionMode, BookmakerSession, BookmakerSessionState, BookmakerSessionStatus,
    BookmakerSessionSyncState,
};
use tracing::{debug, error, info, warn};

use crate::auth::{self, BookmakerAuth, BookmakerSessionMaterial};
use crate::execution::BookmakerExecutionAdapter;
use crate::adapters::pari_api::PariApiClient;

#[derive(Debug, Clone)]
pub struct PariExecutionAdapter {
    /// Enable real API calls (requires valid session cookies)
    enable_real_api: bool,
}

impl Default for PariExecutionAdapter {
    fn default() -> Self {
        Self {
            enable_real_api: std::env::var("PARI_REAL_API")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
        }
    }
}

impl PariExecutionAdapter {
    pub const BOOKMAKER: &'static str = "pari";
    const API_BASE_URL: &'static str = "https://api.pari.ru";

    /// Create adapter with explicit real API flag
    pub fn with_real_api(enabled: bool) -> Self {
        Self {
            enable_real_api: enabled,
        }
    }

    fn dry_run_message(request: &BetExecutionRequest) -> String {
        format!(
            "pari execution stub: dry-run only for {} {} on event {}",
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
                detail: Some("pari session is not configured".into()),
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
                        "pari session configured but not authenticated",
                    ),
                    BookmakerSessionState::Active if has_imported_material => (
                        BookmakerSessionSyncState::Authenticated,
                        true,
                        true,
                        "pari session material is present; credential validity is checked by the read-only API request",
                    ),
                    BookmakerSessionState::Active => (
                        BookmakerSessionSyncState::Configured,
                        false,
                        false,
                        "pari session is marked active but contains no cookie or login material; re-authentication is required",
                    ),
                    BookmakerSessionState::Expired => (
                        BookmakerSessionSyncState::Expired,
                        false,
                        false,
                        "pari session expired",
                    ),
                    BookmakerSessionState::Locked => (
                        BookmakerSessionSyncState::Locked,
                        false,
                        false,
                        "pari session locked",
                    ),
                    BookmakerSessionState::Disconnected => (
                        BookmakerSessionSyncState::Disconnected,
                        false,
                        false,
                        "pari session disconnected",
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
impl BookmakerAuth for PariExecutionAdapter {
    async fn authorize(
        &self,
        _account: &shared::BookmakerAccount,
    ) -> Result<crate::auth::AuthSession, crate::auth::AuthError> {
        Err(crate::auth::AuthError::AuthFailed {
            bookmaker: Self::BOOKMAKER.into(),
            error: "Pari login/bootstrap is not implemented; import a verified session manually".into(),
        })
    }

    async fn check_session(&self, _session: &crate::auth::AuthSession) -> Result<bool, crate::auth::AuthError> {
        // There is no server-side session validation endpoint wired into this adapter.
        Ok(false)
    }
}

#[async_trait]
impl BookmakerExecutionAdapter for PariExecutionAdapter {
    fn capability(&self) -> BookmakerExecutionCapability {
        BookmakerExecutionCapability {
            bookmaker: Self::BOOKMAKER.into(),
            supports_dry_run: true,
            supports_balance_snapshot: true,
            // Balance refresh is implemented, but live coupon placement is not.
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
                remote_balance_fetch_enabled: self.enable_real_api,
                auth: BookmakerAdapterAuthMetadata {
                    flow: "manual_cookie_session".into(),
                    requires_human_bootstrap: true,
                    session_bootstrap_enabled: false,
                    session_refresh_enabled: false,
                    persisted_snapshot_enabled: true,
                },
                readiness: BookmakerAdapterReadinessMetadata {
                    stage: BookmakerAdapterReadinessStage::AuthenticatedReadOnly,
                    safe_mode_only: true,
                    approval_reference_required: false,
                    operator_notes: vec![
                        "session bootstrap and refresh are not implemented; import a session manually".into(),
                        "live balance refresh may be enabled with PARI_REAL_API, but coupon placement remains disabled".into(),
                    ],
                },
                notes: vec![
                    if self.enable_real_api {
                        "read-only API mode: live balance refresh can use imported session material"
                    } else {
                        "safe mode: cached account state only"
                    }
                    .into(),
                    "bet placement is disabled: no live coupon request or bookmaker receipt is implemented".into(),
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

        // Try real API call if enabled and we have session material
        if self.enable_real_api {
            if let Some(material) = session_material {
                if let Some(cookie_header) = &material.cookie_header {
                    info!("Attempting real balance fetch from Pari API");
                    
                    let mut client = PariApiClient::new();
                    
                    // Authenticate with cookie
                    match client.authenticate_with_cookie(cookie_header).await {
                        Ok(_token) => {
                            // Fetch balance
                            match client.get_balance().await {
                                Ok(balance) => {
                                    let available = balance.available_balance.unwrap_or(0.0);
                                    info!("Retrieved real balance from Pari: {}", available);
                                    
                                    let snapshot = BookmakerBalanceSnapshot {
                                        account_id: account.id,
                                        bookmaker: Self::BOOKMAKER.into(),
                                        available_balance: available,
                                        total_balance: balance.total_balance.unwrap_or(available),
                                        bonus_balance: balance.bonus_balance,
                                        currency: balance.currency.unwrap_or_else(|| "RUB".into()),
                                        exposure: 0.0,
                                        captured_at: checked_at,
                                        source: Some("api".into()),
                                    };
                                    
                                    return Ok(BookmakerBalanceRefresh {
                                        account_id: Some(account.id),
                                        bookmaker: Self::BOOKMAKER.into(),
                                        state: BookmakerBalanceRefreshState::RemoteBalanceFetched,
                                        session_status: session_status.clone(),
                                        snapshot: Some(snapshot),
                                        detail: Some("pari balance fetched from live API".into()),
                                        checked_at,
                                    });
                                }
                                Err(e) => {
                                    warn!("Failed to fetch Pari balance from API: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to authenticate with Pari API: {}", e);
                        }
                    }
                }
            }
        }

        // Fall back to cached/mock behavior
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
            .map(BookmakerSessionMaterial::has_usable_cookie_header)
            .unwrap_or(false);
        let detail = match state {
            BookmakerBalanceRefreshState::NoSession => {
                "pari balance refresh skipped: no session configured"
            }
            BookmakerBalanceRefreshState::SessionNotAuthenticated => {
                "pari balance refresh skipped: session is not authenticated"
            }
            BookmakerBalanceRefreshState::AuthenticatedBalanceUnavailable => {
                if has_imported_material {
                    "pari real session material imported; remote balance endpoint failed, no cache"
                } else {
                    "pari session is authenticated but remote balance fetch is disabled in safe mode"
                }
            }
            BookmakerBalanceRefreshState::CachedBalanceAvailable => {
                if has_imported_material {
                    "pari returned cached balance snapshot (API fetch failed or disabled)"
                } else {
                    "pari returned cached balance snapshot; remote refresh remains disabled"
                }
            }
            BookmakerBalanceRefreshState::RemoteBalanceFetched => {
                "pari balance fetched from live API"
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
        // Never report a successful submission until Pari returns a real coupon receipt.
        // The endpoint, event mapping and market/selection mapping are not implemented yet.
        warn!(
            event_id = %request.event_id,
            market = %request.market,
            selection = %request.selection,
            real_api_enabled = self.enable_real_api,
            "Pari bet placement is disabled; no bookmaker request was sent"
        );

        Ok(BetExecutionReceipt {
            ticket_id: None,
            account_id: Some(account.id),
            bookmaker: Self::BOOKMAKER.into(),
            status: BetExecutionStatus::Blocked,
            mode: account.mode.clone(),
            accepted_stake: 0.0,
            accepted_odds: 0.0,
            message: Some(
                "pari bet placement is not implemented; no request was sent and no ticket was created".into(),
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
            bookmaker: PariExecutionAdapter::BOOKMAKER.into(),
            label: "main".into(),
            currency: "RUB".into(),
            enabled: true,
            mode: BookmakerExecutionMode::DryRun,
            created_at: Utc::now(),
            last_used_at: None,
        }
    }

    #[tokio::test]
    async fn authorize_does_not_create_a_fake_session() {
        let adapter = PariExecutionAdapter::default();
        let account = account();
        let error = adapter.authorize(&account).await.expect_err("unimplemented authentication must fail closed");
        assert!(error.to_string().contains("login/bootstrap is not implemented"));
    }

    #[tokio::test]
    async fn check_session_does_not_accept_an_unverified_session() {
        let adapter = PariExecutionAdapter::default();
        let session = crate::auth::AuthSession {
            bookmaker_id: PariExecutionAdapter::BOOKMAKER.into(),
            cookies: crate::auth::SessionCookies::default(),
            created_at: Utc::now(),
            expires_at: None,
        };
        assert!(!adapter.check_session(&session).await.unwrap());
    }

    #[test]
    fn reports_configured_session_as_not_authenticated() {
        let adapter = PariExecutionAdapter::default();
        let account = account();
        let session = BookmakerSession {
            account_id: account.id,
            bookmaker: account.bookmaker.clone(),
            state: BookmakerSessionState::Configured,
            token_hint: Some("cfg...".into()),
            last_synced_at: Utc::now(),
            expires_at: None,
        };

        let status =
            futures::executor::block_on(adapter.get_session_status(&account, Some(&session), None))
                .expect("status should succeed");

        assert_eq!(status.sync_state, BookmakerSessionSyncState::Configured);
        assert!(!status.authenticated);
    }

    #[test]
    fn real_api_flag_enables_balance_refresh_but_never_bet_placement() {
        let safe_adapter = PariExecutionAdapter::default();
        let api_adapter = PariExecutionAdapter::with_real_api(true);
        let safe_capability = safe_adapter.capability();
        let api_capability = api_adapter.capability();

        assert!(!safe_capability.supports_bet_placement);
        assert!(!api_capability.supports_bet_placement);
        assert!(!safe_capability.supports_real_money);
        assert!(!api_capability.supports_real_money);
        assert_eq!(
            api_capability.account_metadata.readiness.stage,
            BookmakerAdapterReadinessStage::AuthenticatedReadOnly
        );
        assert!(!api_capability.account_metadata.auth.session_bootstrap_enabled);
        assert!(!api_capability.account_metadata.auth.session_refresh_enabled);
        assert!(!safe_capability.account_metadata.remote_balance_fetch_enabled);
        assert!(api_capability.account_metadata.remote_balance_fetch_enabled);
    }

    #[test]
    fn active_session_without_credentials_is_not_authenticated() {
        let adapter = PariExecutionAdapter::default();
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
            .expect("status should succeed");

        assert_eq!(status.sync_state, BookmakerSessionSyncState::Configured);
        assert!(!status.authenticated);
        assert!(!status.can_refresh_balance);
    }

    #[test]
    fn place_bet_never_reports_a_submission_without_a_ticket() {
        let adapter = PariExecutionAdapter::with_real_api(true);
        let mut account = account();
        account.mode = BookmakerExecutionMode::Real;
        let request = BetExecutionRequest {
            bookmaker: PariExecutionAdapter::BOOKMAKER.into(),
            event_id: "event-1".into(),
            market: "1X2".into(),
            selection: "1".into(),
            odds: 2.1,
            stake: 500.0,
            allow_dry_run: false,
            reference: Some("operator-approval".into()),
        };
        let receipt = futures::executor::block_on(adapter.place_bet(&account, &request))
            .expect("blocked placement should return a receipt");
        assert_eq!(receipt.status, BetExecutionStatus::Blocked);
        assert!(receipt.ticket_id.is_none());
        assert_eq!(receipt.accepted_stake, 0.0);
        assert_eq!(receipt.accepted_odds, 0.0);
        assert!(receipt.message.as_deref().unwrap_or_default().contains("no request was sent"));
    }
}
