<script>
  import { t } from './i18n.svelte.js';
  import { ENDPOINTS, total, fmt, dayLabel } from './usage.js';

  /** `daily`: [{day, counts}] oldest first (the last 30 days). */
  let { daily = [] } = $props();

  const H = 120;
  const W = 600;
  const GAP = 2;
  const values = $derived(daily.map((d) => total(d.counts)));
  const max = $derived(Math.max(1, ...values));
  // A round axis maximum: 1, 2 or 5 times a power of ten.
  const top = $derived.by(() => {
    const p = 10 ** Math.floor(Math.log10(max));
    return [1, 2, 5, 10].map((m) => m * p).find((v) => v >= max);
  });
  const bw = $derived(daily.length ? W / daily.length : W);
  const empty = $derived(values.every((v) => v === 0));

  let hover = $state(null);
  let box = $state(null);

  function bar(v) {
    const h = v ? Math.max(2, (v / top) * H) : 0;
    return { h, y: H - h };
  }

  /** Bar with 4px rounded top corners, square at the baseline. */
  function path(x, w, y, h) {
    if (h <= 0) return '';
    const r = Math.min(4, w / 2, h);
    return `M${x},${H}V${y + r}Q${x},${y} ${x + r},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${H}Z`;
  }

  function move(e) {
    const rect = box.getBoundingClientRect();
    const i = Math.floor(((e.clientX - rect.left) / rect.width) * daily.length);
    hover = i >= 0 && i < daily.length ? { i, x: e.clientX - rect.left } : null;
  }
</script>

<div class="chart" bind:this={box} role="img" aria-label={t('usage.chartLabel')} onpointermove={move} onpointerleave={() => (hover = null)}>
  <div class="axis-top small muted">{fmt(top)}</div>
  <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" aria-hidden="true">
    <line x1="0" x2={W} y1={H / 2} y2={H / 2} class="grid" />
    <line x1="0" x2={W} y1="0" y2="0" class="grid" />
    {#each daily as d, i (d.day)}
      {@const b = bar(values[i])}
      <path d={path(i * bw + GAP / 2, bw - GAP, b.y, b.h)} class="bar" class:dim={hover && hover.i !== i} />
    {/each}
    <line x1="0" x2={W} y1={H} y2={H} class="base" />
  </svg>
  <div class="axis-x small muted">
    {#if daily.length}
      <span>{dayLabel(daily[0].day)}</span>
      <span>{dayLabel(daily[Math.floor(daily.length / 2)].day)}</span>
      <span>{t('usage.today')}</span>
    {/if}
  </div>
  {#if empty}
    <div class="empty-note small muted">{t('usage.none')}</div>
  {/if}
  {#if hover}
    {@const d = daily[hover.i]}
    <div class="tip" style="left: {Math.min(Math.max(hover.x, 80), (box?.clientWidth || 0) - 80)}px">
      <div class="tip-head">{dayLabel(d.day, { weekday: 'short', month: 'short', day: 'numeric' })} · {fmt(values[hover.i])}</div>
      {#each ENDPOINTS as ep (ep)}
        {#if d.counts[ep]}
          <div class="tip-row">
            <span class="mono">/{ep}</span>
            <span>{fmt(d.counts[ep].ok)}{#if d.counts[ep].fail} <span class="fail">+{fmt(d.counts[ep].fail)} {t('usage.failed')}</span>{/if}</span>
          </div>
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .chart {
    position: relative;
    padding-top: 18px;
    touch-action: pan-y;
  }

  svg {
    display: block;
    width: 100%;
    height: 140px;
    overflow: visible;
  }

  .bar {
    fill: var(--info);
    transition: opacity 0.12s;
  }

  .bar.dim {
    opacity: 0.45;
  }

  .grid {
    stroke: var(--border);
    stroke-dasharray: 3 4;
    vector-effect: non-scaling-stroke;
  }

  .base {
    stroke: var(--muted);
    stroke-opacity: 0.5;
    vector-effect: non-scaling-stroke;
  }

  .axis-top {
    position: absolute;
    top: 0;
    left: 0;
  }

  .axis-x {
    display: flex;
    justify-content: space-between;
    margin-top: 4px;
  }

  .empty-note {
    position: absolute;
    inset: 18px 0 24px;
    display: grid;
    place-items: center;
  }

  .tip {
    position: absolute;
    top: 0;
    transform: translateX(-50%);
    min-width: 160px;
    padding: 8px 10px;
    border-radius: 9px;
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.14);
    font-size: 12px;
    pointer-events: none;
    z-index: 5;
  }

  .tip-head {
    font-weight: 600;
    margin-bottom: 4px;
  }

  .tip-row {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    font-variant-numeric: tabular-nums;
  }

  .fail {
    color: var(--muted);
  }
</style>
