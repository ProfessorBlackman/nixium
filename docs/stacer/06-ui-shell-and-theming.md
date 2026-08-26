# 06 — UI Shell, Navigation and Theming

## 1. Window structure

`stacer/app.ui` defines a frameless-looking two-column layout inside a `QMainWindow`:

```
┌──────────┬──────────────────────────────────────────────────────┐
│ sidebar  │  pageTitle                                           │
│          ├──────────────────────────────────────────────────────┤
│ btnDash  │                                                      │
│ btnStar… │   pageContentLayout ← SlidingStackedWidget           │
│ btnSyst… │      (holds all 10–12 page widgets)                  │
│ …        │                                                      │
│ btnSett… │                                                      │
│ btnFeed… │                                                      │
└──────────┴──────────────────────────────────────────────────────┘
```

- `horizontalLayout` margins and spacing are zeroed in `App::init()`.
- The sidebar gets a drop shadow with alpha 60 (`Utilities::addDropShadow`).
- The window is centred on the primary screen's available geometry at startup.
- Sidebar buttons are checkable `QPushButton`s (`btnDash`, `btnStartupApps`,
  `btnSystemCleaner`, `btnSearch`, `btnServices`, `btnProcesses`, `btnHelpers`,
  `btnUninstaller`, `btnResources`, `btnAptSourceManager`, `btnGnomeSettings`, `btnSettings`,
  `btnFeedback`). Their **tooltips are the page names** and are used as identifiers by the
  tray menu and page lookup.
- The header label `pageTitle` is set from `widget->windowTitle()` on every page change.

## 2. Page navigation

`App::pageClick(QWidget *widget, bool slide = true)`:

```cpp
ui->pageTitle->setText(widget->windowTitle());
if (slide) mSlidingStacked->slideInIdx(mSlidingStacked->indexOf(widget));
else       mSlidingStacked->setCurrentWidget(widget);
```

Each `on_btnX_clicked()` slot simply calls `pageClick(xPage)`. Auto-connection by name
(`on_<object>_<signal>`) is used throughout the codebase.

`App::clickSidebarButton(pageTitle, isShow)` is the indirect path used by the tray menu and the
start-page preference: it resolves a page **by matching `windowTitle()` against a string**,
falls back to the first page when no match is found, sets the matching sidebar button checked
by tooltip, then shows and activates the window if requested.

Because the lookup key is a translated string, changing the language invalidates a stored
`StartPage` preference and the app silently opens the Dashboard.

### `SlidingStackedWidget`

`stacer/sliding_stacked_widget.{h,cpp}` — a `QStackedWidget` subclass with an animated
transition, a common Qt-forum utility class:

- `speed = 150 ms`, `animationtype = QEasingCurve::Linear`, `vertical = false` by default.
- Directions: `LEFT2RIGHT`, `RIGHT2LEFT`, `TOP2BOTTOM`, `BOTTOM2TOP`, `AUTOMATIC`.
  `AUTOMATIC` derives the direction from index order (higher index slides in from the right).
- Re-entrant calls are rejected while an animation is `active`.
- `slideInIdx` wraps out-of-range indices modulo `count()`.
- Emits `animationFinished()`.

Used twice: for the main page stack, and inside the GNOME Settings page for its three
sub-pages.

## 3. System tray

Created in `AppManager` (`QSystemTrayIcon` with `:/static/themes/default/img/sidebar-icons/dash.png`)
and populated in `App::createTrayActions()`:

- One `QAction` per sidebar button, labelled with the button's tooltip; triggering it calls
  `clickSidebarButton(toolTip, true)` — which also un-hides and activates the window.
- A separator, then a Quit action calling `qApp->quit()`.
- **A `QSystemTrayIcon::activated` connection is made inside the per-button loop**, so clicking
  the tray icon fires N identical show/activate handlers (harmless but redundant).

The tray icon is also the delivery channel for threshold alerts (`showMessage`, see
[07-feature-dashboard.md](07-feature-dashboard.md)).

## 4. Close behaviour

`App::closeEvent`:

- If `AppQuitDialogDontAsk` is set: honour `AppQuitDialogChoice` — `"close"` accepts the event,
  anything else ignores it and hides the window.
- Otherwise show a `QMessageBox` ("Will the program continue to work in the system tray?")
  with two custom buttons — **Quit** (accessible name `danger`) and **Continue** (accessible
  name `primary`) — and a "Don't ask again." checkbox wired directly to
  `SettingManager::setAppQuitDialogDontAsk`.
- The chosen answer is persisted as `AppQuitDialogChoice` (`"close"` / `"hide"`).

Note the `accessibleName` values (`danger`, `primary`, `circle`) are used as **QSS selectors**
(`QPushButton[accessibleName="danger"]`) — that is how the theme styles semantic button roles.

## 5. Styling system

Two layers:

1. **QSS** — one large stylesheet per theme
   (`static/themes/default/style/style.qss`, ~1070 lines; `light/style.qss`, ~1030 lines)
   applied application-wide via `qApp->setStyleSheet()`. Widgets are targeted by class,
   `objectName`, and `accessibleName`.
2. **Colour tokens** — `values.ini` per theme, a flat `@token=#rrggbb` map. `AppManager`
   textually substitutes every token in the QSS before applying it.

Token set (identical in both themes):

```
@pageContent  @sidebar  @circleChartBackgroundColor  @historyChartBackgroundColor
@chartLabelColor  @chartGridColor  @color01 … @color16
```

Qt Charts widgets cannot be styled by QSS, so `CircleBar`, `HistoryChart` and the disk pie
chart subscribe to `SignalMapper::sigChangedAppTheme()` and pull colours from
`AppManager::ins()->getStyleValues()` (the live `QSettings` for the current theme) to set
background brushes, label colours and grid-line colours imperatively.

**Theme switching is disabled in 1.1.0.** `SettingManager::getThemeName()` returns the literal
`"default"` (the `QSettings` read is commented out), `AppManager::loadThemeList()` and the
Settings-page combo box are commented out. The `light` theme assets and `themes.json` ship but
are unreachable. Any rebuild should treat light/dark as a *feature to restore*, and the token
list above is the palette contract.

## 6. Shared UI helpers

`stacer/utilities.h` (header-only):

```cpp
static void addDropShadow(QWidget *w, int alpha, int blur = 15);
static void addDropShadow(QList<QWidget*> ws, int alpha, int blur = 15);
static QString getDesktopValue(const QRegExp &key, const QStringList &lines);
```

- `addDropShadow` attaches a `QGraphicsDropShadowEffect` (offset 0, black at the given alpha).
  It is called on almost every card, button and table in the app — this is the source of the
  app's "material card" look. Alpha values in use: 30, 40, 50, 55, 60.
- `getDesktopValue` is the `.desktop` file mini-parser: filter lines by regex, split the first
  match on `=`, return the trimmed value. Used by Startup Apps and the Settings autostart
  toggle. Note it splits on `=` and takes `last()`, so values containing `=` are truncated.

## 7. Reusable widgets

| Widget | File | Role |
| --- | --- | --- |
| `CircleBar` | `Pages/Dashboard/circlebar.*` | Donut gauge (Qt Charts `QPieSeries`, hole 0.67, −115°…115°, conical gradient) with a title and a centre value label |
| `LineBar` | `Pages/Dashboard/linebar.*` | `QProgressBar` + rate label + total label |
| `HistoryChart` | `Pages/Resources/history_chart.*` | 60-second spline chart with N series, legend as labels, optional `QCategoryAxis` for byte-formatted Y labels, collapse checkbox |
| `ByteTreeWidget` | `Pages/SystemCleaner/byte_tree_widget.*` | `QTreeWidgetItem` that sorts column 1 numerically by a hidden byte value in role `0x0100` |
| `ServiceItem` | `Pages/Services/service_item.*` | One service row: name, description, two toggles |
| `StartupApp` | `Pages/StartupApps/startup_app.*` | One autostart row: name, enable toggle, edit, delete |
| `APTSourceRepositoryItem` | `Pages/AptSourceManager/…` | One repo row: enable toggle + rendered source line |
| `SlidingStackedWidget` | `stacer/sliding_stacked_widget.*` | Animated page container |

The list-of-custom-widgets pattern is used consistently: a `QListWidget` gets an empty
`QListWidgetItem` per row whose `sizeHint` is taken from a custom widget set via
`setItemWidget()`. This is why those lists cannot be sorted or filtered by Qt's model layer —
each page implements filtering by hiding items manually.

## 8. Empty and loading states

- Every list page has a `notFoundWidget` shown when the list is empty (with
  `static/themes/common/img/not-found.png`).
- Long operations swap a button for a `QLabel` displaying an animated `QMovie`
  (`loading.gif`, `scanLoading.gif`, `loadings.gif`), then swap back.
- The GIF paths are theme-interpolated, so they are (re)created inside the
  `sigChangedAppTheme` handler on the System Cleaner page.

## 9. Internationalisation in the UI

- All user-visible strings go through `tr()`; `.ui` files are translated by `AUTOUIC` + Linguist.
- `AppManager` installs a `QTranslator` for `stacer_<lang>` from
  `applicationDirPath()/translations` and sets `Qt::RightToLeft` for `ar`.
- The language combo is populated from `:/static/languages.json` (24 entries).
- Changing language writes the setting but **does not re-translate the running UI** — it takes
  effect on next launch (no `retranslateUi` pass is performed).
