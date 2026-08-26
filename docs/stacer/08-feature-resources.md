# 08 — Feature: Resources (history charts)

**Files:** `stacer/Pages/Resources/{resources_page,history_chart}.{h,cpp,ui}`
**Window title:** `Resources` · **Sidebar button:** `btnResources`

## Layout

A vertical stack (`chartsLayout`, inside a container named `charts`) of six cards:

| Card | Series | Y axis |
| --- | --- | --- |
| History of CPU | one per **logical** core | 0–100 (%) |
| History of CPU Load Averages | 3 (1/5/15 min) | 0–max(ceil(avg), coreCount) |
| History of Disk Read Write | 2 (read, write) | `QCategoryAxis`, byte-formatted |
| History of Memory | 2 (swap, memory) | 0–100 (%) |
| History of Network | 2 (download, upload) | `QCategoryAxis`, byte-formatted |
| File System (disk pie) | one slice per mounted volume | — |

All six are refreshed by a single `QTimer` at **1000 ms**, connected to five slots
(`updateCpuChart`, `updateCpuLoadAvg`, `updateDiskReadWrite`, `updateMemoryChart`,
`updateNetworkChart`); the pie chart is built once and only rebuilt when its filters change.
Like the Dashboard, the timer starts in the constructor and never stops.

Colour palette (shared by `HistoryChart` and the pie chart, 20 entries):

```
0x2ecc71 0xe74c3c 0x3498db 0xf1c40f 0xe67e22 0x1abc9c 0x9b59b6 0x34495e 0xd35400 0xc0392b
0x8e44ad 0xFF8F00 0xEF6C00 0x4E342E 0x424242 0x5499C7 0x58D68D 0xCD6155 0xF5B041 0x566573
```

A machine with more than 20 logical cores indexes past the end of this list — the pie chart
guards with `i < chartColors.count() ? … : i - chartColors.count()`, `HistoryChart` does not
(`colors.at(i)` asserts).

## `HistoryChart`

```cpp
HistoryChart(const QString &title, int seriesCount,
             QCategoryAxis *categoryAxisY = nullptr, QWidget *parent = nullptr);
QVector<QSplineSeries*> getSeriesList() const;
void setSeriesList(const QVector<QSplineSeries*> &);
void setYMax(int);
void setCategoryAxisYLabels();
QCategoryAxis *getAxisY();
```

Construction:
- creates `seriesCount` `QSplineSeries` and adds them to the chart,
- assigns colours from the palette,
- `createDefaultAxes()`, then X axis range `0..60` **reversed** (so x=0 is "now" on the right),
- margins tightened (`setContentsMargins(-11,-11,-11,-11)`, `setMargins(20,0,10,10)`),
- subscribes to `sigChangedAppTheme` to set label colour, grid colour, background brush and
  legend label colour from `values.ini`.

The card header has a title label (`lblHistoryTitle`) and a checkbox (`checkHistoryTitle`)
that acts as a **"maximise this chart"** toggle: `on_checkHistoryTitle_clicked(checked)` walks
`topLevelWidget()->findChild<QWidget*>("charts")->layout()` and hides every sibling chart,
then re-shows itself. Unchecking restores all of them. The disk pie card re-implements the same
handler inline.

### The 60-point scrolling buffer

Every update slot performs the same manual shift, e.g. for CPU:

```cpp
static int second = 0;
for (each series j) {
    for (int i = 0; i < (second < 61 ? second : 61); i++)
        series[j]->replace(i, i + 1, series[j]->at(i).y());   // move each point one x to the right
    series[j]->insert(0, QPointF(0, newValue));               // new sample at x = 0
    series[j]->setName(<label with the current value>);        // legend doubles as a readout
    if (second > 61) series[j]->removePoints(61, 1);           // drop the point that fell off
}
second++;
```

So the chart is a hand-rolled 61-sample ring buffer over the point list, with the **series name
used as the live numeric readout** (the legend shows e.g. `CPU3: 42%`,
`Download: 1.2 MiB/s Total: 4.1 GiB`). Each slot keeps its own `static int second` counter.

`setYMax(v)` sets `axisY()->setRange(0, v)`. For the byte-valued charts,
`setCategoryAxisYLabels()` clears the category axis and re-appends four labels at
`max/4 * i`, formatted with `FormatUtil::formatBytes` — that is how the Y axis shows
`1.5 MiB` instead of `1572864`.

`setSeriesList()` is effectively a no-op repaint helper — see
[20-known-quirks-and-bugs.md](20-known-quirks-and-bugs.md) (it replaces index 0 in a loop).
The charts update anyway because the slots mutate the same `QSplineSeries` objects the chart
already owns.

## Per-chart data sources

| Chart | Source | Derivation |
| --- | --- | --- |
| CPU | `InfoManager::getCpuPercents()` | uses `cpuPercents[j+1]`, i.e. skips the aggregate line — one series per core |
| Load average | `getCpuLoadAvgs()` | raw 1/5/15-minute values; Y max grows to `max(ceil(avg), coreCount)` |
| Disk R/W | `getDiskIO()` | `Δbytes` per tick from `/sys/block/*/stat`; Y max floor `100 KiB`, monotonically grows |
| Memory | `updateMemoryInfo()` then `getSwapUsed/Total`, `getMemUsed/Total` | series 0 = swap %, series 1 = memory %; labels carry absolute bytes |
| Network | `getRXbytes/getTXbytes()` | `Δbytes` per tick; Y max floor 1 MiB, `qMax` of both directions, monotonically grows |

The memory chart divides by `getMemTotal()` **without a zero guard** (the swap series is
guarded); this only matters if `/proc/meminfo` parsing fails.

## Disk pie chart ("File System")

Built in `initDiskPieChart()` as a hand-assembled card (a `QWidget` + `QGridLayout`, not a
`.ui` form) containing:

- a title label `lblChartTitle` = `tr("File System")`,
- a `checkHistoryTitle` maximise checkbox (same behaviour as the history cards),
- a spacer,
- `cmbDevice` — `tr("Device")` plus `InfoManager::getDevices()`,
- `cmbFileSystemType` — `tr("File System Type")` plus `InfoManager::getFileSystemTypes()`,
- the `QChartView` spanning the row below.

One slice per `Disk`, valued by **total size** (not usage). `diskPieSeriesCustomize()` assigns
palette colours, a light-grey border, and a hover handler that explodes the slice and rewrites
the chart title to `"<name> (<formatted size>) (<percent>)"`.

Changing either combo box removes the series, rebuilds it filtered by the selected device or
filesystem type (index 0 = "all"), re-customises, **emits `sigChangedAppTheme()`** to reapply
colours, and re-adds the series. Animations are `QChart::AllAnimations`; minimum height 500 px.

Because `DiskInfo` does not filter pseudo-filesystems, the unfiltered pie includes every
tmpfs, squashfs snap mount and overlay — the two combos exist to make it readable.

## Port notes

- Replace the manual point shifting with a proper ring buffer / deque in the backend and send
  the whole window to the frontend, or stream one sample and let the chart library handle it.
- Series-name-as-readout should become explicit label state.
- Y-axis maxima should use a decaying/rolling maximum rather than an all-time high.
- Filter pseudo-filesystems by default (keep a "show all" option) instead of relying on the
  user to pick a device.
- The "maximise one chart" behaviour is a layout concern; in the rebuild it is a UI state flag,
  not sibling-widget traversal.
