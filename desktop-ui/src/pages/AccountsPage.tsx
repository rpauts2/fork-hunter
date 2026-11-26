import { useState } from 'react'
import { motion, AnimatePresence } from 'framer-motion'
import { AlertTriangle, CheckCircle2, ChevronDown, Cookie, Database, Plus, RefreshCw, Shield, X } from 'lucide-react'
import type {
  AccountControlUpdate,
  AccountSessionImportPayload,
  AccountSessionSummary,
  AccountStateResponse,
  BankrollState,
} from '../types'

interface AccountsPageProps {
  accounts: AccountStateResponse[]
  accountsSummary: AccountSessionSummary | null
  bankrollState: BankrollState | null
  onBootstrap: (bookmaker: string, login?: string, sessionHint?: string, payload?: AccountSessionImportPayload) => Promise<AccountStateResponse | null>
  onRefreshBalance: (bookmaker: string) => Promise<AccountStateResponse | null>
  onUpdateControl: (bookmaker: string, update: AccountControlUpdate) => Promise<AccountStateResponse | null>
}

const formatMoney = (amount: number | null | undefined, currency = 'RUB') => {
  if (amount == null || !Number.isFinite(amount)) return '—'
  return new Intl.NumberFormat('ru-RU', { style: 'currency', currency, maximumFractionDigits: 0 }).format(amount)
}

const relativeTime = (value: string | null | undefined) => {
  if (!value) return 'нет данных'
  const timestamp = new Date(value).getTime()
  if (!Number.isFinite(timestamp)) return 'нет данных'
  const minutes = Math.max(0, Math.floor((Date.now() - timestamp) / 60000))
  if (minutes < 1) return 'только что'
  if (minutes < 60) return minutes + ' мин назад'
  if (minutes < 1440) return Math.floor(minutes / 60) + ' ч назад'
  return Math.floor(minutes / 1440) + ' дн назад'
}

const readinessLabel = (account: AccountStateResponse) => {
  if (account.readiness.placement_ready) return 'Размещение доступно'
  if (account.readiness.dry_run_ready) return 'Готов к dry-run'
  if (account.readiness.session_ready) return 'Сессия есть, исполнение ограничено'
  if (account.session_material?.cookie_header_present) return 'Cookie импортированы, нужна проверка'
  return 'Не подключён'
}

export function AccountsPage({
  accounts, accountsSummary, bankrollState,
  onBootstrap, onRefreshBalance, onUpdateControl,
}: AccountsPageProps) {
  const [expanded, setExpanded] = useState<string | null>(null)
  const [importBookmaker, setImportBookmaker] = useState<string | null>(null)
  const [cookieHeader, setCookieHeader] = useState('')
  const [csrfToken, setCsrfToken] = useState('')
  const [userAgent, setUserAgent] = useState('')
  const [busyBookmaker, setBusyBookmaker] = useState<string | null>(null)

  const openImport = (bookmaker: string) => {
    setImportBookmaker(bookmaker)
    setCookieHeader('')
    setCsrfToken('')
    setUserAgent('')
  }

  const saveImport = async () => {
    if (!importBookmaker || !cookieHeader.trim()) return
    setBusyBookmaker(importBookmaker)
    try {
      const payload: AccountSessionImportPayload = {
        cookieHeader: cookieHeader.trim(),
        csrfToken: csrfToken.trim() || undefined,
        userAgent: userAgent.trim() || undefined,
      }
      const result = await onBootstrap(importBookmaker, undefined, undefined, payload)
      if (result) {
        setImportBookmaker(null)
        setCookieHeader('')
        setCsrfToken('')
        setUserAgent('')
      }
    } finally {
      setBusyBookmaker(null)
    }
  }

  const createSafeMode = async (bookmaker: string) => {
    setBusyBookmaker(bookmaker)
    try {
      await onBootstrap(bookmaker)
    } finally {
      setBusyBookmaker(null)
    }
  }

  const refreshBalance = async (bookmaker: string) => {
    setBusyBookmaker(bookmaker)
    try {
      await onRefreshBalance(bookmaker)
    } finally {
      setBusyBookmaker(null)
    }
  }

  const toggleAccount = async (account: AccountStateResponse) => {
    if (!account.account) return
    setBusyBookmaker(account.bookmaker)
    try {
      await onUpdateControl(account.bookmaker, { enabled: !account.account.enabled })
    } finally {
      setBusyBookmaker(null)
    }
  }

  return (
    <motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold text-text-primary">Аккаунты букмекеров</h1>
          <p className="text-sm text-text-secondary mt-1">Реальное состояние аккаунтов и безопасные операции. Реальные ставки отключены.</p>
        </div>
        <div className="flex items-center gap-2 text-xs text-amber-400">
          <Shield size={15} /> Safe mode
        </div>
      </div>

      {accountsSummary && (
        <div className="grid grid-cols-2 xl:grid-cols-5 gap-3">
          {[
            ['Настроено', accountsSummary.accounts_configured],
            ['Включено', accountsSummary.accounts_enabled],
            ['Авторизовано', accountsSummary.sessions_authenticated],
            ['Баланс в кэше', accountsSummary.balances_cached],
            ['Готовы к dry-run', accountsSummary.ready_for_dry_run],
          ].map(([label, value]) => (
            <div key={String(label)} className="rounded-card border border-border bg-surface p-4">
              <div className="text-xs text-text-muted">{label}</div>
              <div className="text-2xl font-bold text-text-primary mt-1">{value}</div>
            </div>
          ))}
        </div>
      )}

      {bankrollState && (
        <div className="rounded-card border border-border bg-surface p-4 flex flex-wrap items-center gap-x-8 gap-y-2 text-sm">
          <div><span className="text-text-muted">Банкролл: </span><b>{formatMoney(bankrollState.total_budget)}</b></div>
          <div><span className="text-text-muted">Экспозиция: </span><b>{formatMoney(bankrollState.total_exposure)}</b></div>
          <div><span className="text-text-muted">Дневной результат: </span><b>{formatMoney(bankrollState.daily_profit)}</b></div>
        </div>
      )}

      <div className="space-y-3">
        {accounts.map(account => {
          const isExpanded = expanded === account.bookmaker
          const isBusy = busyBookmaker === account.bookmaker
          const currency = account.balance?.currency ?? account.account?.currency ?? 'RUB'
          return (
            <section key={account.bookmaker} className="rounded-card border border-border bg-surface overflow-hidden">
              <div className="p-4 flex flex-wrap items-center gap-4">
                <div className="w-10 h-10 rounded-xl bg-background border border-border flex items-center justify-center text-text-secondary font-semibold">
                  {account.bookmaker.slice(0, 2).toUpperCase()}
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <h2 className="font-semibold text-text-primary">{account.bookmaker}</h2>
                    <span className={'text-xs px-2 py-0.5 rounded-full ' + (account.readiness.session_ready ? 'bg-emerald-500/10 text-emerald-400' : 'bg-amber-500/10 text-amber-400')}>
                      {readinessLabel(account)}
                    </span>
                  </div>
                  <p className="text-xs text-text-muted mt-1">
                    {account.account ? account.account.label + ' • ' + account.account.mode : 'Аккаунт не создан'}
                    {account.session ? ' • Сессия: ' + account.session.state : ''}
                  </p>
                </div>
                <div className="text-right">
                  <div className="text-sm font-semibold text-text-primary">{formatMoney(account.balance?.available_balance, currency)}</div>
                  <div className="text-[11px] text-text-muted">Доступный баланс</div>
                </div>
                <button onClick={() => setExpanded(isExpanded ? null : account.bookmaker)} className="p-2 text-text-muted hover:text-text-primary">
                  <ChevronDown size={18} className={isExpanded ? 'rotate-180' : ''} />
                </button>
              </div>

              <AnimatePresence>
                {isExpanded && (
                  <motion.div initial={{ height: 0, opacity: 0 }} animate={{ height: 'auto', opacity: 1 }} exit={{ height: 0, opacity: 0 }} className="overflow-hidden">
                    <div className="border-t border-border p-4 space-y-4">
                      <div className="grid grid-cols-1 md:grid-cols-3 gap-4 text-sm">
                        <div>
                          <div className="text-xs text-text-muted">Поддержка</div>
                          <div className="mt-1">{account.capability.supports_dry_run ? 'Dry-run: да' : 'Dry-run: нет'}</div>
                          <div>{account.capability.supports_balance_snapshot ? 'Снимок баланса: да' : 'Снимок баланса: нет'}</div>
                          <div>{account.capability.supports_bet_placement ? 'Размещение: поддерживается' : 'Размещение: не реализовано'}</div>
                        </div>
                        <div>
                          <div className="text-xs text-text-muted">Сессия</div>
                          <div className="mt-1">Cookie: {account.session_material?.cookie_header_present ? 'импортированы' : 'нет'}</div>
                          <div>Последняя синхронизация: {relativeTime(account.session?.last_synced_at)}</div>
                          <div>Баланс обновлён: {relativeTime(account.balance?.captured_at)}</div>
                        </div>
                        <div>
                          <div className="text-xs text-text-muted">Ограничения</div>
                          <div className="mt-1">Real money: {account.capability.supports_real_money && account.readiness.real_money_enabled ? 'разрешён' : 'отключён'}</div>
                          <div>Approval: {account.readiness.approval_required ? 'требуется' : 'не требуется'}</div>
                          <div>Safe mode: {account.readiness.submit_blocked_by_safe_mode ? 'блокирует отправку' : '—'}</div>
                        </div>
                      </div>

                      {(account.readiness.operator_action || account.readiness.blocking_reasons.length > 0 || account.control_issues.length > 0) && (
                        <div className="rounded-lg bg-amber-500/5 border border-amber-500/20 p-3 text-xs text-amber-300 space-y-1">
                          {account.readiness.operator_action && <p>{account.readiness.operator_action}</p>}
                          {account.readiness.blocking_reasons.map((reason, index) => <p key={index}>• {reason}</p>)}
                          {account.control_issues.map((issue, index) => <p key={'control-' + index}>• {issue}</p>)}
                        </div>
                      )}

                      <div className="flex flex-wrap gap-2">
                        {!account.account && (
                          <button disabled={isBusy} onClick={() => createSafeMode(account.bookmaker)} className="btn btn-secondary text-xs flex items-center gap-2">
                            <Plus size={14} /> Создать safe-mode аккаунт
                          </button>
                        )}
                        <button disabled={isBusy} onClick={() => openImport(account.bookmaker)} className="btn btn-secondary text-xs flex items-center gap-2">
                          <Cookie size={14} /> Импортировать cookie
                        </button>
                        <button disabled={isBusy || !account.session || !account.capability.supports_balance_snapshot} onClick={() => refreshBalance(account.bookmaker)} className="btn btn-secondary text-xs flex items-center gap-2">
                          <RefreshCw size={14} /> Обновить баланс
                        </button>
                        {account.account && (
                          <button disabled={isBusy} onClick={() => toggleAccount(account)} className="btn btn-secondary text-xs">
                            {account.account.enabled ? 'Отключить аккаунт' : 'Включить аккаунт'}
                          </button>
                        )}
                      </div>
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>
            </section>
          )
        })}

        {!accounts.length && (
          <div className="rounded-card border border-border bg-surface py-14 text-center">
            <Database size={32} className="mx-auto text-text-muted opacity-40 mb-3" />
            <p className="text-sm text-text-secondary">Backend пока не вернул список аккаунтов</p>
            <p className="text-xs text-text-muted mt-1">Демо-профили и фиктивные балансы больше не отображаются</p>
          </div>
        )}
      </div>

      <div className="rounded-card border border-amber-500/20 bg-amber-500/5 p-4 flex gap-3 text-sm text-amber-200">
        <AlertTriangle size={18} className="shrink-0 mt-0.5" />
        <div>
          <b>Реальное размещение ставок отключено.</b>
          <p className="text-xs text-amber-200/80 mt-1">Импорт cookie и обновление баланса не означают, что букмекер подтвердил действительность сессии или готов принять ставку. Не включайте реальные ставки в обход этих ограничений.</p>
        </div>
      </div>

      <AnimatePresence>
        {importBookmaker && (
          <motion.div className="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}>
            <motion.div initial={{ y: 12, opacity: 0 }} animate={{ y: 0, opacity: 1 }} exit={{ y: 12, opacity: 0 }} className="w-full max-w-xl rounded-card border border-border bg-surface p-5 space-y-4">
              <div className="flex items-center justify-between">
                <div>
                  <h2 className="font-semibold text-text-primary">Импорт сессии: {importBookmaker}</h2>
                  <p className="text-xs text-text-muted mt-1">Cookie передаётся backend и не сохраняется в localStorage.</p>
                </div>
                <button onClick={() => setImportBookmaker(null)} className="p-2 text-text-muted hover:text-text-primary"><X size={18} /></button>
              </div>
              <label className="block text-xs text-text-secondary">
                Cookie header
                <textarea value={cookieHeader} onChange={event => setCookieHeader(event.target.value)} rows={5} className="input w-full mt-1 font-mono text-xs" placeholder="session=..." />
              </label>
              <label className="block text-xs text-text-secondary">
                User-Agent (необязательно)
                <input value={userAgent} onChange={event => setUserAgent(event.target.value)} className="input w-full mt-1" placeholder="Mozilla/5.0 ..." />
              </label>
              <label className="block text-xs text-text-secondary">
                CSRF token (необязательно)
                <input value={csrfToken} onChange={event => setCsrfToken(event.target.value)} className="input w-full mt-1 font-mono" />
              </label>
              <div className="flex justify-end gap-2">
                <button onClick={() => setImportBookmaker(null)} className="btn btn-secondary text-sm">Отмена</button>
                <button disabled={!cookieHeader.trim() || busyBookmaker === importBookmaker} onClick={saveImport} className="btn btn-primary text-sm">
                  {busyBookmaker === importBookmaker ? 'Сохраняю…' : 'Импортировать'}
                </button>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </motion.div>
  )
}
