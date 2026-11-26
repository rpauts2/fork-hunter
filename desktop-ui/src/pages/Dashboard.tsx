import { motion } from 'framer-motion'
import { Activity, AlertTriangle, ArrowUpRight, CheckCircle2, Clock3, DollarSign, Radio, Target, Timer, Wallet, Zap } from 'lucide-react'
import { StatCard } from '../components/StatCard'
import type { AccountSessionSummary, BankrollState, Bookmaker, ParserHealth, ScannerMetrics, ScannerStatus, Surebet } from '../types'

interface DashboardProps {
  connected: boolean
  scannerStatus: ScannerStatus | null
  metrics: ScannerMetrics | null
  surebets: Surebet[]
  bookmakers: Bookmaker[]
  parserHealth: ParserHealth[]
  accountsSummary: AccountSessionSummary | null
  bankrollState: BankrollState | null
  onOpenSurebets: () => void
}

const container = { hidden: { opacity: 0 }, show: { opacity: 1, transition: { staggerChildren: 0.05 } } }
const item = { hidden: { opacity: 0, y: 16 }, show: { opacity: 1, y: 0, transition: { duration: 0.25 } } }

const formatMoney = (amount: number | null | undefined) =>
  amount == null || !Number.isFinite(amount) ? '—' : new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 0 }).format(amount) + ' ₽'

const formatRelativeTime = (dateStr: string | null | undefined) => {
  if (!dateStr) return 'нет данных'
  const timestamp = new Date(dateStr).getTime()
  if (!Number.isFinite(timestamp)) return 'нет данных'
  const minutes = Math.max(0, Math.floor((Date.now() - timestamp) / 60000))
  if (minutes < 1) return 'только что'
  if (minutes < 60) return minutes + ' мин назад'
  if (minutes < 1440) return Math.floor(minutes / 60) + ' ч назад'
  return Math.floor(minutes / 1440) + ' дн назад'
}

const sportLabel = (sport: string) => ({
  Football: 'Футбол',
  Tennis: 'Теннис',
  Hockey: 'Хоккей',
  Basketball: 'Баскетбол',
}[sport] ?? sport)

const healthLabel = (status: string) => {
  const normalized = status.toLowerCase()
  if (normalized.includes('healthy') || normalized === 'ok') return 'Работает'
  if (normalized.includes('degraded') || normalized.includes('warning')) return 'Есть ограничения'
  if (normalized.includes('disabled')) return 'Отключён'
  return 'Нет подтверждения'
}

const healthColor = (status: string) => {
  const normalized = status.toLowerCase()
  if (normalized.includes('healthy') || normalized === 'ok') return 'text-emerald-400'
  if (normalized.includes('degraded') || normalized.includes('warning')) return 'text-amber-400'
  return 'text-text-muted'
}

export function Dashboard({
  connected, scannerStatus, metrics, surebets, bookmakers, parserHealth,
  accountsSummary, bankrollState, onOpenSurebets,
}: DashboardProps) {
  const recentSurebets = [...surebets].sort((a, b) => b.profitPercent - a.profitPercent).slice(0, 5)
  const enabledBookmakers = bookmakers.filter(bookmaker => bookmaker.enabled && bookmaker.scan_supported)
  const verifiedCount = surebets.filter(surebet => surebet.verified).length
  const averageEdge = surebets.length
    ? surebets.reduce((sum, surebet) => sum + surebet.profitPercent, 0) / surebets.length
    : null

  return (
    <motion.div variants={container} initial="hidden" animate="show" className="space-y-6">
      <motion.div variants={item} className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold text-text-primary">Обзор сканера</h1>
          <p className="text-sm text-text-secondary mt-1">Показываются данные backend. Демонстрационные сигналы отключены.</p>
        </div>
        <div className="flex items-center gap-2 text-xs text-text-secondary">
          <span className={"w-2 h-2 rounded-full " + (connected ? 'bg-emerald-400' : 'bg-amber-400')} />
          WebSocket {connected ? 'подключён' : 'не подключён'}
          <span className="text-text-muted">•</span>
          {scannerStatus?.running ? 'Сканер запущен' : 'Сканер не подтверждён'}
          <span className="text-text-muted">•</span>
          Обновление {formatRelativeTime(metrics?.timestamp)}
        </div>
      </motion.div>

      <div className="grid grid-cols-1 sm:grid-cols-2 xl:grid-cols-4 gap-4">
        <motion.div variants={item}><StatCard icon={DollarSign} label="Профит за день" value={formatMoney(bankrollState?.daily_profit)} color="green" /></motion.div>
        <motion.div variants={item}><StatCard icon={Target} label="Вилки в выдаче" value={metrics?.surebets_found ?? surebets.length} trend={verifiedCount + ' проверено'} trendUp={verifiedCount > 0} color="blue" /></motion.div>
        <motion.div variants={item}><StatCard icon={Activity} label="События обработаны" value={metrics?.events_parsed ?? '—'} color="purple" /></motion.div>
        <motion.div variants={item}><StatCard icon={Timer} label="Время цикла" value={metrics ? (metrics.cycle_time_ms / 1000).toFixed(2) + ' с' : '—'} color="orange" /></motion.div>
      </div>

      <div className="grid grid-cols-1 xl:grid-cols-3 gap-6">
        <div className="xl:col-span-2 space-y-6">
          <motion.section variants={item} className="rounded-card border border-border bg-surface p-5">
            <div className="flex items-center justify-between gap-3 mb-4">
              <div>
                <h2 className="font-semibold text-text-primary">Последние найденные вилки</h2>
                <p className="text-xs text-text-muted mt-1">Сигналы из текущего backend</p>
              </div>
              <button onClick={onOpenSurebets} className="text-xs text-accent hover:text-accent-hover flex items-center gap-1">
                Все сигналы <ArrowUpRight size={14} />
              </button>
            </div>
            {recentSurebets.length ? (
              <div className="space-y-3">
                {recentSurebets.map(surebet => (
                  <div key={surebet.id} className="rounded-lg bg-background border border-border/70 p-3">
                    <div className="flex flex-wrap items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="font-medium text-sm text-text-primary">{surebet.home_team} — {surebet.away_team}</div>
                        <div className="text-xs text-text-secondary mt-1">
                          {surebet.league} • {sportLabel(surebet.sport)} • {surebet.legs[0]?.market ?? 'Рынок не указан'}
                        </div>
                      </div>
                      <div className="text-right shrink-0">
                        <div className="text-sm font-bold text-emerald-400">+{surebet.profitPercent.toFixed(2)}%</div>
                        <div className="text-[11px] text-text-muted">{formatMoney(surebet.total_stake)}</div>
                      </div>
                    </div>
                    <div className="flex flex-wrap items-center gap-2 mt-3">
                      {surebet.legs.map((leg, index) => (
                        <span key={surebet.id + '-' + index} className="text-xs px-2 py-1 rounded bg-surface border border-border text-text-secondary">
                          {leg.bookmaker}: {leg.selection} @ {leg.odds.toFixed(2)}
                        </span>
                      ))}
                      <span className={"text-[11px] ml-auto " + (surebet.verified ? 'text-emerald-400' : 'text-amber-400')}>
                        {surebet.verified ? 'Проверена' : 'Не проверена'}
                      </span>
                    </div>
                  </div>
                ))}
              </div>
            ) : (
              <div className="py-10 text-center">
                <Target size={30} className="mx-auto text-text-muted opacity-40 mb-3" />
                <p className="text-sm text-text-secondary">Backend пока не вернул вилки</p>
                <p className="text-xs text-text-muted mt-1">Тестовые и демонстрационные сигналы не подставляются</p>
              </div>
            )}
          </motion.section>

          <motion.section variants={item} className="rounded-card border border-border bg-surface p-5">
            <div className="flex items-center gap-2 mb-4"><Wallet size={17} className="text-text-secondary" /><h2 className="font-semibold text-text-primary">Банкролл</h2></div>
            {bankrollState ? (
              <>
                <div className="grid grid-cols-2 md:grid-cols-4 gap-4 mb-5">
                  <div><p className="text-xs text-text-muted">Общий бюджет</p><p className="font-semibold mt-1">{formatMoney(bankrollState.total_budget)}</p></div>
                  <div><p className="text-xs text-text-muted">Доступно</p><p className="font-semibold mt-1">{formatMoney(bankrollState.total_budget - bankrollState.total_exposure)}</p></div>
                  <div><p className="text-xs text-text-muted">Экспозиция</p><p className="font-semibold mt-1">{formatMoney(bankrollState.total_exposure)}</p></div>
                  <div><p className="text-xs text-text-muted">Общий результат</p><p className="font-semibold mt-1">{formatMoney(bankrollState.total_profit)}</p></div>
                </div>
                <div className="space-y-2">
                  {bankrollState.bookmakers.map(balance => (
                    <div key={balance.bookmaker} className="flex items-center justify-between text-sm border-t border-border/60 pt-2">
                      <span className="text-text-secondary">{balance.bookmaker}</span>
                      <span className="text-text-primary">{formatMoney(balance.available)} доступно</span>
                    </div>
                  ))}
                </div>
              </>
            ) : <p className="text-sm text-text-muted py-5">Данные банкролла пока не получены от backend</p>}
          </motion.section>
        </div>

        <div className="space-y-6">
          <motion.section variants={item} className="rounded-card border border-border bg-surface p-5">
            <div className="flex items-center gap-2 mb-4"><Radio size={17} className="text-text-secondary" /><h2 className="font-semibold text-text-primary">Источники данных</h2></div>
            <div className="space-y-3">
              {parserHealth.length ? parserHealth.slice(0, 12).map(parser => (
                <div key={parser.bookmaker} className="flex items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="text-sm text-text-primary">{parser.bookmaker}</div>
                    <div className="text-[11px] text-text-muted">{parser.events_parsed} событий • {formatRelativeTime(parser.last_success)}</div>
                  </div>
                  <span className={"text-xs shrink-0 " + healthColor(parser.status)}>{healthLabel(parser.status)}</span>
                </div>
              )) : enabledBookmakers.map(bookmaker => (
                <div key={bookmaker.slug} className="flex items-center justify-between gap-3">
                  <span className="text-sm text-text-primary">{bookmaker.name}</span>
                  <span className="text-xs text-text-muted">{bookmaker.backend_status ?? 'статус не указан'}</span>
                </div>
              ))}
              {!parserHealth.length && !enabledBookmakers.length && <p className="text-sm text-text-muted">Нет данных об источниках</p>}
            </div>
          </motion.section>

          <motion.section variants={item} className="rounded-card border border-border bg-surface p-5">
            <div className="flex items-center gap-2 mb-4"><CheckCircle2 size={17} className="text-text-secondary" /><h2 className="font-semibold text-text-primary">Готовность аккаунтов</h2></div>
            {accountsSummary ? (
              <div className="space-y-3 text-sm">
                <div className="flex justify-between"><span className="text-text-secondary">Настроено</span><b>{accountsSummary.accounts_configured}</b></div>
                <div className="flex justify-between"><span className="text-text-secondary">Сессии авторизованы</span><b>{accountsSummary.sessions_authenticated}</b></div>
                <div className="flex justify-between"><span className="text-text-secondary">Баланс в кэше</span><b>{accountsSummary.balances_cached}</b></div>
                <div className="flex justify-between"><span className="text-text-secondary">Готовы к dry-run</span><b>{accountsSummary.ready_for_dry_run}</b></div>
                <div className="flex justify-between"><span className="text-text-secondary">Готовы к исполнению</span><b>{accountsSummary.ready_for_execution}</b></div>
                {accountsSummary.accounts_with_control_issues > 0 && (
                  <div className="flex items-center gap-2 text-amber-400 text-xs pt-2 border-t border-border"><AlertTriangle size={14} />Есть проблемы управления: {accountsSummary.accounts_with_control_issues}</div>
                )}
              </div>
            ) : <p className="text-sm text-text-muted">Сводка аккаунтов недоступна</p>}
          </motion.section>

          <motion.section variants={item} className="rounded-card border border-border bg-surface p-5">
            <div className="flex items-center gap-2 mb-3"><Zap size={17} className="text-text-secondary" /><h2 className="font-semibold text-text-primary">Качество сигналов</h2></div>
            <div className="text-2xl font-bold text-text-primary">{averageEdge == null ? '—' : averageEdge.toFixed(2) + '%'}</div>
            <p className="text-xs text-text-muted mt-1">Средняя маржа по текущей выдаче, не реализованный ROI</p>
            <div className="text-xs text-text-secondary mt-4 flex items-center gap-2"><Clock3 size={13} />Последний цикл: {metrics ? metrics.cycle_time_ms + ' мс' : 'нет данных'}</div>
          </motion.section>
        </div>
      </div>
    </motion.div>
  )
}
