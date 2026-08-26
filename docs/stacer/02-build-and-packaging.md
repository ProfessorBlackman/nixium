# 02 — Build, Resources and Packaging

## 1. CMake (primary)

Top level `CMakeLists.txt`:

```cmake
cmake_minimum_required(VERSION 3.1)
project(Stacer)
include(cmake/cxxbasics/CXXBasics.cmake)     # ccache/sccache, faster linkers, Debug default
set(CMAKE_BINARY_DIR "${CMAKE_BINARY_DIR}/output")
set(EXECUTABLE_OUTPUT_PATH "${CMAKE_BINARY_DIR}/")
set(LIBRARY_OUTPUT_PATH    "${CMAKE_BINARY_DIR}/lib")
set(CMAKE_AUTOMOC ON)
find_package(Qt5 COMPONENTS Core Gui Widgets Charts Svg Concurrent REQUIRED)
set(CMAKE_CXX_STANDARD 11)   # extensions ON, standard required
add_definitions(-DQT_DEPRECATED_WARNINGS)
add_subdirectory(stacer-core)
add_subdirectory(stacer)
```

Documented build (README):

```sh
mkdir build && cd build
cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH=/qt/path/bin ..
make -j $(nproc)
output/bin/stacer
```

`cmake/cxxbasics/` is a vendored third-party helper collection (UNLICENSE) providing
`UseCCache`, `UseSCCache`, `UseFasterLinkers`, `GetTargetArch`, and a Debug-by-default policy.
It is a build convenience only — nothing in it affects runtime behaviour.

### stacer-core target

```cmake
include_directories(. Info Tools Utils)
file(GLOB_RECURSE srcs "**.cpp")
add_definitions(-DSTACERCORE_LIBRARY)
find_package(Qt5 COMPONENTS Core Network REQUIRED)
add_library(stacer-core STATIC ${srcs})
target_link_libraries(stacer-core Qt5::Core Qt5::Network)
```

`stacer-core_global.h` defines `STACERCORESHARED_EXPORT` as `Q_DECL_EXPORT` when
`STACERCORE_LIBRARY` is set, else `Q_DECL_IMPORT`. Since the library is built **static**, the
export macro is effectively vestigial.

### stacer target

```cmake
include_directories(${PROJECT_ROOT}/stacer-core . Managers
                    Pages/{Dashboard,Processes,Resources,Services,Settings,
                           StartupApps,SystemCleaner,Uninstaller}
                    ${CMAKE_CURRENT_BINARY_DIR})     # for generated ui_*.h
file(GLOB_RECURSE srcs "**.cpp")
file(GLOB_RECURSE translations "${PROJECT_ROOT}/translations/**.ts")
find_package(Qt5LinguistTools)
qt5_create_translation(QM_FILES ${translations} ${srcs})
set(CMAKE_AUTOUIC ON)
set(CMAKE_AUTORCC ON)
add_executable(stacer ${srcs} static.qrc ${QM_FILES})
target_link_libraries(stacer stacer-core Qt5::Core Qt5::Gui Qt5::Widgets
                             Qt5::Charts Qt5::Svg Qt5::Concurrent)
```

Notes that matter for a rewrite:

- `Pages/AptSourceManager`, `Pages/GnomeSettings`, `Pages/Search`, `Pages/Helpers` are **not**
  in `include_directories`; those pages use path-qualified includes instead.
- `-flto` is added only for `Release` + GNU compiler.
- `install()` rules are guarded to `Release|RelWithDebInfo|MinSizeRel` — an unoptimised build
  cannot be installed. They install `bin/stacer`, `share/applications/stacer.desktop`, and
  `stacer/static/logo.png` → `share/icons/stacer.png`.

## 2. qmake (legacy, kept in tree)

`Stacer.pro` is `TEMPLATE = subdirs` over `stacer-core` and `stacer`.
`stacer-core.pro` declares `QT -= gui`, `QT += core network`, `TEMPLATE = lib`,
`DEFINES += STACERCORE_LIBRARY`, and lists sources explicitly (so it can drift from the CMake
glob). `stacer/stacer.pro` declares `QT += core gui charts svg concurrent` plus `widgets`.
`release.sh` still calls `lupdate`/`lrelease` on `stacer/stacer.pro`, so the `.pro` files are
required for translation generation even in a CMake build.

## 3. Qt resource system (`static.qrc`)

Everything under `stacer/static/` that the UI references is compiled into the binary under the
`:/` prefix (AUTORCC). ~78 entries: two QSS stylesheets, two `values.ini` colour token files,
the `Ubuntu-R.ttf` font, `languages.json`, `themes.json`, `logo.png`, `splashscreen.png`, and
per-theme images including three animated GIFs used as spinners.

Layout convention:

```
static/
├── font/Ubuntu-R.ttf
├── languages.json          # [{value: "en", text: "English"}, …] 24 entries
├── themes.json             # [{value:"default"}, {value:"light"}]  (loader is commented out)
├── logo.png  splashscreen.png
└── themes/
    ├── common/img/…        # theme-agnostic icons (checkbox, delete, folder, package, …)
    ├── default/
    │   ├── img/            # incl. sidebar-icons/*.png, loading.gif, scanLoading.gif
    │   └── style/{style.qss, values.ini}
    └── light/
        ├── img/            # partial override set
        └── style/{style.qss, values.ini}
```

Resource paths are built at runtime by string interpolation, e.g.
`QString(":/static/themes/%1/img/loading.gif").arg(themeName)` — which is why the `light`
theme must duplicate every image the code interpolates, and why a missing file yields a
silently blank icon.

## 4. Theming pipeline

`AppManager::updateStylesheet()`:

1. `appThemePath = ":/static/themes/<themeName>/style"`.
2. Load `values.ini` into a `QSettings` (INI format) — a flat list of `@token=#rrggbb` pairs.
3. Read `style.qss` as a string.
4. For **every** key in the INI, `replace(key, value)` over the whole stylesheet — a plain
   textual substitution of `@pageContent`, `@color01` … `@color16`, etc.
5. `qApp->setStyleSheet(...)`, then `emit SignalMapper::sigChangedAppTheme()`.

`default/style/values.ini` (dark) defines: `@pageContent`, `@sidebar`,
`@circleChartBackgroundColor`, `@historyChartBackgroundColor`, `@chartLabelColor`,
`@chartGridColor`, `@color01`…`@color16`. The `light` theme defines the same key set.
Chart widgets can't be styled by QSS, so they read the same `QSettings` object directly via
`AppManager::ins()->getStyleValues()` when `sigChangedAppTheme` fires.

Substitution is order-dependent and prefix-unsafe: `@color1` would also match inside
`@color10`. The existing token names avoid the collision by being fixed-width (`@color01`).

## 5. Translations

- 26 `translations/stacer_<locale>.ts` files (ar, ca-es, cs, de, en, es, fr, gl, hi, hu, it,
  kn, ko, ml, nl, oc, pl, pt, ro, ru, sv, tr, ua, vn, zh-cn, zh-tw).
  `languages.json` lists 24 — `gl` and `ro` have `.ts` files but no entry, so they are not
  selectable in the UI.
- CMake regenerates `.qm` via `qt5_create_translation`; `release.sh` uses
  `lupdate -no-obsolete` + `lrelease`.
- At runtime `AppManager` loads `stacer_<lang>` from
  `qApp->applicationDirPath() + "/translations"` — i.e. **next to the executable**, not from
  the resource system or a share path. A system install into `/usr/bin` therefore finds no
  translations; the `.deb` works because it installs everything into
  `/usr/share/stacer/` and symlinks the binary.
- `ar` additionally switches the whole app to `Qt::RightToLeft`.

## 6. Desktop integration

`applications/stacer.desktop`:

```ini
[Desktop Entry]
Name=Stacer
Exec=stacer
Comment=Linux System Optimizer and Monitoring
Icon=stacer
Type=Application
Terminal=false
Categories=Utility;
```

`icons/hicolor/{16x16,32x32,64x64,128x128,256x256}/apps/stacer.png` for the icon theme.

## 7. Release and Debian packaging

`release.sh` (`VERSION=1.1.0`):

1. `cmake -DCMAKE_BUILD_TYPE=debug …` then `make -j nproc`  (note: builds **debug**, while the
   CMake `install()` rules refuse debug — the script sidesteps `install` by copying files).
2. Copy `icons/ applications/ debian/` into `Release/stacer-1.1.0/`, and `build/output/*`
   into `Release/stacer-1.1.0/stacer/`.
3. `lupdate`/`lrelease`, move the `.qm` files to `…/stacer/translations`.
4. Download `linuxdeployqt` AppImage and bundle non-Qt libs
   (`-bundle-non-qt-libs -no-translations -unsupported-allow-new-glibc`), after clearing
   `QTDIR`, `QT_PLUGIN_PATH`, `LD_LIBRARY_PATH`.
5. If invoked as `release.sh deb`: `dh_make --createorig -i -c mit` then
   `debuild --no-lintian -us -uc`.

`debian/control`: `Package: stacer`, `Architecture: all`, `Recommends: systemd, curl`.
`debian/install` maps `stacer/* → usr/share/stacer/`, `applications/* → usr/share/applications/`,
`icons/* → usr/share/icons/`.
`debian/postinst` symlinks `/usr/share/stacer/stacer → /usr/bin/stacer`;
`debian/postrm` unlinks it and `rm -rf /usr/share/stacer`.

So the shipped layout is a **self-contained bundle directory** (Qt libs + translations
alongside the binary) plus a symlink — not a normal FHS install. The README also documents
PPA, AUR, `.deb`, `.rpm`, and `dnf install stacer` paths, and lists `curl, systemd` as the
required packages.

`.travis.yml` and `.github/FUNDING.yml` are present; CI only built the project.

## 8. Runtime files the app creates

| Path | Written by |
| --- | --- |
| `~/.config/stacer/settings.ini` | `SettingManager` |
| `~/.config/stacer/stacer.log` | `main.cpp` logger (dead code — handler never installed) |
| `~/.config/autostart/stacer.desktop` | Settings → "start on boot" |
| `~/.config/autostart/<name>.desktop` | Startup Apps page (arbitrary entries) |
| `/tmp/stacer_etc_host_new_content` | Hosts editor staging file, then `pkexec mv` to `/etc/hosts` |
| `~/.local/share/Trash/{files,info}/…` | Search page "Move Trash" |
