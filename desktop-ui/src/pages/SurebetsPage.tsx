import { useMemo, useState } from 'react'
import { motion } from 'framer-motion'
import { RefreshCw, Flame, Search, Timer, CheckCircle2, AlertTriangle } from 'lucide-react'
import type { Surebet } from '../types'

interface SurebetsPageProps {
  surebets: Surebet[]
  connected: boolean
  onRefresh: () => void
}

const container = { hidden: { opacity: 0 }, show: { opacity: 1, transition: { staggerChildren: 0.03 } } }
const item = { hidden: { opacity: 0, y: 12 }, show: { opacity: 1, y: 0, transition: { duration: 0.2 } } }

const formatMoney = (amount: number) =>
  new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 0 }).format(amount) + ' ₽'

const sportLabel = (sport: string) => ({
  Football: 'Футбол',
  Tennis: 'Теннис',
  Hockey: 'Хоккей',
  Basketball: 'Баскетбол',
}[sport] ?? sport)

const formatStartTime = (value: string | null) => {
  if (!value) return 'Время неизвестно'
  const date = new Date(value)
  if (!Number.isFinite(date.getTime())) return 'Время неизвестно'
  return date.toLocaleString('ru-RU', { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' })
}

const timeLeft = (value: string | null) => {
  if (!value) return null
  const seconds = Math.floor((new Date(value).getTime() - Date.now()) / 1000)
  if (!Number.isFinite(seconds)) return null
  if (seconds < 0) return 'Началось'
  if (seconds < 3600) return Math.floor(seconds / 60) + ' мин'
  return Math.floor(seconds / 3600) + ' ч ' + Math.floor((seconds % 3600) / 60) + ' мин'
}

const profitColor = (profit: number) => {
  if (profit >= 5) return 'text-emerald-400 bg-emerald-500/10 border-emerald-500/30'
  if (profit >= 3) return 'text-blue-400 bg-blue-500/10 border-blue-500/30'
  if (profit >= 1) return 'text-amber-400 bg-amber-500/10 border-amber-500/30'
  return 'text-text-secondary bg-background border-border'
}

export function SurebetsPage({ surebets, connected, onRefresh }: SurebetsPageProps) {
  const [search, setSearch] = useState('')
  const [selectedSport, setSelectedSport] = useState('Все виды')
  const [selectedBookmaker, setSelectedBookmaker] = useState('Все БК')
  const [selectedProfit, setSelectedProfit] = useState('Все')
  const [selectedTime, setSelectedTime] = useState('Все')
  const [hotOnly, setHotOnly] = useState(false)
  const sports = useMemo(() => ['Все виды', ...new Set(surebets.map(surebet => sportLabel(surebet.sport)))], [surebets])
  const bookmakers = useMemo(() => ['Все БК', ...new Set(surebets.flatMap(surebet => surebet.legs.map(leg => leg.bookmaker)))], [surebets])

  const filtered = useMemo(() => surebets.filter(surebet => {
    const query = search.trim().toLowerCase()
    if (query && ![surebet.home_team, surebet.away_team, surebet.league].some(value => value.toLowerCase().includes(query))) return false
    if (selectedSport !== 'Все виды' && sportLabel(surebet.sport) !== selectedSport) return false
    if (selectedBookmaker !== 'Все БК' && !surebet.legs.some(leg => leg.bookmaker === selectedBookmaker)) return false
    if (selectedProfit !== 'Все') {
      const min = Number(selectedProfit.replace('>', '').replace('%', ''))
      if (surebet.profitPercent < min) return false
    }
    if (hotOnly && surebet.profitPercent < 5) return false
    if (selectedTime !== 'Все') {
      if (!surebet.start_time) return false
      const start = new Date(surebet.start_time)
      const now = new Date()
      if (selectedTime === 'След. час' && (start.getTime() < now.getTime() || start.getTime() > now.getTime() + 3600000)) return false
      if (selectedTime === 'Сегодня' && start.toDateString() !== now.toDateString()) return false
      if (selectedTime === 'Завтра') {
        const tomorrow = new Date(now)
        tomorrow.setDate(now.getDate() + 1)
        if (start.toDateString() !== tomorrow.toDateString()) return false
      }
    }
    return true
  }), [surebets, search, selectedSport, selectedBookmaker, selectedProfit, selectedTime, hotOnly])
    .sort((a, b) => b.profitPercent - a.profitPercent)

  const verifiedCount = surebets.filter(surebet => surebet.verified).length

  return (
    <motion.div variants={container} initial="hidden" animate="show" className="space-y-6">
      <motion.div variants={item} className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold text-text-primary">Арбитражные вилки</h1>
          <p className="text-sm text-text-secondary mt-1">
            {filtered.length} из {surebets.length} сигналов • {verifiedCount} проверено
          </p>
        </div>
        <div className="flex items-center gap-3">
          <span className="text-xs text-text-muted">{connected ? 'Live-соединение активно' : 'WebSocket отключён, действует API polling'}</span>
          <button onClick={onRefresh} className="btn btn-secondary text-sm flex items-center gap-2">
            <RefreshCw size={15} /> Обновить
          </button>
        </div>
      </motion.div>

      <motion.div variants={item} className="flex flex-wrap items-center gap-3">
        <div className="relative">
          <Search size={16} className="absolute left-3 top-1/2 -translate-y-1/2 text-text-muted" />
          <input type="text" placeholder="Поиск команд, лиг..." value={search} onChange={event => setSearch(event.target.value)} className="input pl-9 w-64" />
        </div>
        <select value={selectedSport} onChange={event => setSelectedSport(event.target.value)} className="input w-36">
          {sports.map(sport => <option key={sport}>{sport}</option>)}
        </select>
        <select value={selectedBookmaker} onChange={event => setSelectedBookmaker(event.target.value)} className="input w-36">
          {bookmakers.map(bookmaker => <option key={bookmaker}>{bookmaker}</option>)}
        </select>
        <select value={selectedProfit} onChange={event => setSelectedProfit(event.target.value)} className="input w-28">
          {['Все', '>1%', '>3%', '>5%', '>10%'].map(value => <option key={value}>{value}</option>)}
        </select>
        <select value={selectedTime} onChange={event => setSelectedTime(event.target.value)} className="input w-32">
          {['Все', 'След. час', 'Сегодня', 'Завтра'].map(value => <option key={value}>{value}</option>)}
        </select>
        <button onClick={() => setHotOnly(value => !value)} className={'btn text-sm flex items-center gap-2 ' + (hotOnly ? 'btn-primary' : 'btn-secondary')}>
          <Flame size={15} /> Профит ≥5%
        </button>
      </motion.div>

      <motion.div variants={item} className="rounded-card border border-border bg-surface overflow-hidden">
        <div className="hidden lg:grid grid-cols-12 gap-4 px-5 py-3 bg-background border-b border-border text-xs text-text-secondary uppercase tracking-wider">
          <div className="col-span-3">Матч</div>
          <div className="col-span-2">Рынок</div>
          <div className="col-span-1 text-center">Маржа</div>
          <div className="col-span-4">Исходы и коэффициенты</div>
          <div className="col-span-1 text-right">Сумма</div>
          <div className="col-span-1 text-right">Статус</div>
        </div>

        <div className="divide-y divide-border/50">
          {filtered.map(surebet => (
            <div key={surebet.id} className="grid grid-cols-1 lg:grid-cols-12 gap-3 lg:gap-4 px-5 py-4 items-center hover:bg-elevated/20 transition-colors">
              <div className="lg:col-span-3 min-w-0">
                <div className="flex items-center gap-2">
                  {surebet.profitPercent >= 5 && <Flame size={14} className="text-orange-400 shrink-0" />}
                  <span className="text-sm font-medium text-text-primary">{surebet.home_team} — {surebet.away_team}</span>
                </div>
                <div className="text-xs text-text-secondary mt-1">{surebet.league} • {sportLabel(surebet.sport)}</div>
                <div className="text-[11px] text-text-muted mt-1">{surebet.is_live ? 'LIVE' : formatStartTime(surebet.start_time)}</div>
              </div>
              <div className="lg:col-span-2 text-sm text-text-secondary">{surebet.legs[0]?.market ?? '—'}</div>
              <div className="lg:col-span-1 lg:text-center">
                <span className={'inline-flex px-2 py-1 rounded-full text-xs font-bold border ' + profitColor(surebet.profitPercent)}>
                  +{surebet.profitPercent.toFixed(2)}%
                </span>
              </div>
              <div className="lg:col-span-4 flex flex-wrap gap-2">
                {surebet.legs.map((leg, index) => (
                  <div key={surebet.id + '-' + index} className="rounded-md bg-background border border-border px-2 py-1.5 text-xs">
                    <div className="text-text-primary font-medium">{leg.bookmaker}</div>
                    <div className="text-text-secondary">{leg.selection} @ {leg.odds.toFixed(2)}</div>
                    <div className="text-text-muted">{formatMoney(leg.stake)}</div>
                  </div>
                ))}
              </div>
              <div className="lg:col-span-1 lg:text-right text-sm font-medium text-text-primary">{formatMoney(surebet.total_stake)}</div>
              <div className="lg:col-span-1 lg:text-right">
                <div className={'text-xs flex items-center lg:justify-end gap-1 ' + (surebet.verified ? 'text-emerald-400' : 'text-amber-400')}>
                  {surebet.verified ? <CheckCircle2 size={13} /> : <AlertTriangle size={13} />}
                  {surebet.verified ? 'Проверена' : 'Не проверена'}
                </div>
                <div className="text-[11px] text-text-muted mt-1 lg:text-right flex items-center lg:justify-end gap-1">
                  <Timer size={11} /> {timeLeft(surebet.start_time) ?? '—'}
                </div>
              </div>
            </div>
          ))}
        </div>

        {filtered.length === 0 && (
          <div className="py-16 text-center">
            <Search size={42} className="mx-auto text-text-muted opacity-30 mb-4" />
            <div className="text-text-secondary">{surebets.length ? 'По заданным фильтрам вилок нет' : 'Backend пока не вернул ни одной вилки'}</div>
            <div className="text-sm text-text-muted mt-1">Демонстрационные записи не используются</div>
          </div>
        )}
      </motion.div>
    </motion.div>
  )
}
