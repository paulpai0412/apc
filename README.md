# AuTHRity

Advanced Process Control in Rust over OPC UA.

AuTHRity is an industrial Advanced Process Control (APC) platform for deploying and operating DMC/MPC controllers against live plant tags through OPC UA. It is designed for the full controller lifecycle: model creation, simulation, deployment, supervision, and operator visibility.

## 本機水泥磨工作區：新手入口

先閱讀 [繁體中文新手操作手冊](START_HERE.zh-TW.html)。下載後以瀏覽器開啟這個單一 HTML，即可離線查看啟動方式、系統流程、M1–M9／R1–R7 功能、逐步 SOP、安全停止、問題處理及管理者重建程序；不需要啟動文件伺服器。

- 已準備的本機研究入口：<http://127.0.0.1:43180/>。
- 已準備的本機 Runtime 入口：<http://127.0.0.1:43181/>。
- 網址可以開啟就直接使用，不重複啟動。找不到工作區或剛從 GitHub 下載時，先由管理者依手冊準備環境；Git repo 不包含本機二進位、資料庫、憑證或帳密。

這是隔離合成水泥磨的操作指南，不是工廠投運授權。下方通用開發說明及歷史畫面不取代此工作區的入口、權限與安全 SOP。

### 隨 repo 提供的 cement-v4 合成資料與操作影片

下載整份 repo 後，直接用瀏覽器開啟 `START_HERE.zh-TW.html`。GitHub 的檔案頁不會執行 HTML；影片若不能預覽，請使用 **Download raw file** 下載後播放。不需要 Git LFS。

| 內容 | 位置與用途 |
|---|---|
| 完整合成資料 | [`validation/examples/cement-v4/`](validation/examples/cement-v4/)：原工作區 `data/` 的七個檔案，保留原始 bytes。 |
| 原始 CSV | [`cement-2026-q1-raw.csv`](validation/examples/cement-v4/cement-2026-q1-raw.csv)：2026 年第一季、60 秒採樣、129,601 列；包含明示的重複、亂序、品質不良及缺值，供清洗 SOP 使用。 |
| 清洗及工程範本 | [`cleaning.json`](validation/examples/cement-v4/cleaning.json)、[`campaign.json`](validation/examples/cement-v4/campaign.json)、[`plant.json`](validation/examples/cement-v4/plant.json)。 |
| 原生歷史及情境 | [`native-history.json`](validation/examples/cement-v4/native-history.json)、[`scenario.json`](validation/examples/cement-v4/scenario.json)：各 129,600 個採樣時刻。 |
| 資料來源與 SHA-256 | [`provenance.json`](validation/examples/cement-v4/provenance.json)：六個資料／設定檔的原始雜湊與刻意加入的資料缺陷。 |
| 操作影片 | [`cement-closed-loop.mp4`](assets/cement-v4/cement-closed-loop.mp4)：12 分 08.3 秒、1080p、中文合成配音、燒錄繁體字幕、16 章節。 |
| 影片配套 | [SRT 字幕](assets/cement-v4/cement-closed-loop.zh-TW.srt)、[章節時間](assets/cement-v4/chapters.json)、[剪接來源索引](assets/cement-v4/edit-decision-list.json)、[檔案雜湊與聲音來源](assets/cement-v4/publication.json)。 |

這些是隔離合成製程資料與原 React 實錄，不是工廠數據或投運證明。CSV 可直接依手冊 M2 匯入；這些檔案不是可直接 `start` 的完整工作區。`cleaning.json` 的最大補值缺口為 120 秒，影片所展示的清洗版本為 180 秒，兩者不可混稱為同一設定。

影片中的等待剪接及停格均有標示，不等於真實控制週期。剪接索引內的絕對路徑是製作當時的本機來源記錄；原始錄影、運行回條、資料庫與帳密未上傳。

**影片配音使用範圍另列：**影片僅作非商業技術展示。聲音由 Piper 的 `zh_CN-chaowen-medium` 合成，非真人配音。[Chaowen 模型卡](https://huggingface.co/rhasspy/piper-voices/raw/main/zh/zh_CN/chaowen/medium/MODEL_CARD) 列資料集為 CC0，來源為 [Open Home Foundation／Nabu Casa voice-datasets](https://github.com/OHF-Voice/voice-datasets)，並註明微調自 [Xiao Ya](https://huggingface.co/rhasspy/piper-voices/raw/main/zh/zh_CN/xiao_ya/medium/MODEL_CARD)，後者標示資料集限非商業使用。專案程式碼的 Apache-2.0 授權不代表已取得第三方聲音的商用權利；商用或再散布前須另確認適用條款。本 repo 不散布語音模型或其訓練資料。

## Quick Overview

This system provides a practical path from model to closed-loop execution:

- **Controller execution**: `apc_engine` runs the DMC loop each scan, reads CV/MV/DV states from OPC UA, solves constrained moves, and writes setpoints when operating mode and MV mode conditions are valid.
- **Operational supervision**: `controller_host` handles deployment/versioned model storage and starts/stops controller instances as managed processes.
- **Education and system testing**: `virtual_plant` simulates representative unit behavior (Debutanizer/CSTR) for learning APC workflows and testing the AuTHRity stack behavior end-to-end.
- **Operator and engineer workflow**: `hmi` provides real-time visibility, tuning, model management, and deployment actions through a Rust backend + React frontend.
- **Interoperability**: `opcua_server` exposes/hosts process nodes so the stack can integrate with DCS/PLC environments using standard OPC UA patterns.

The design separates concerns across services so control logic, supervision, simulation, and UI can evolve independently while sharing a consistent tag model.

## Node-RED Bridge (Optional)

Node-RED can be used as an OPC UA ⇄ OPC DA translation layer between AuTHRity and legacy DCS systems.
This provides a practical way to handle mode handshakes and write mapping without building a custom bridge service.

## Architecture Overview

```
┌─────────────────┐      OPC UA       ┌──────────────────┐
│     DCS/PLC     │◄────────────────► │  OPC UA Server   │
│                 │                   │                  │
└─────────────────┘                   └───────────────┬──┘
                                         ▲            │
                                  OPC UA │            │ OPC UA
                                         ▼            │
┌─────────────────┐           ┌──────────────────┐    │
│ Controller Host │──────────►│    APC Engine    │    │
│  (Supervisor)   │   spawns  │  (Child Process) │    │
└─────────────────┘           └──────────────────┘    │
         ▲                                            │
         │ HTTP (Control API)                         │
         │                                            │
┌────────┴───────────────────────────────┐            │
│         HMI Backend (Rust)             │◄───────────┘
│  • WebSocket Server                    │
│  • OPC UA Bridge                       │◄────►┌───────────────────────┐
└────────────────────────────────────────┘      │       QuestDB         │
         ▲                                      │  • Process Historian  │ 
         │                                      │  • Trends             │
         │ WebSocket/HTTP                       │  • step-response      │                                   
         ▼                                      └───────────────────────┘
┌────────────────────────────────────────┐
│       HMI Frontend (React/TS)          │
│  • Process Tables & Tuning             │
│  • Real-time Visualization             │
└────────────────────────────────────────┘
```

## Screenshots

Live screenshots from local runs:

### HMI Overview
![AuTHRity HMI Screenshot](assets/HMIscreenshot.png)

### Plant Overview
![Plant Overview](assets/plant_overview.png)

### Model Management
![Models Overview](assets/models%20overview.png)

### Model Generator (Setup)
![Model Generator](assets/modelgenerator1.png)

### Model Generator (Visualization)
![Generator Visualization](assets/generator%20visuallize.png)

### Prediction & Control Horizon (Pass Balancing)
![Prediction and Control Horizon](assets/prediction_control.png)

### FOPDT Physics View
![FOPDT Physics](assets/fopdt%20physics.png)

## What Is Here

- `opcua_server`: OPC UA server with YAML node hot-reload API.
- `virtual_plant`: Debutanizer + CSTR simulator writing OPC UA tags.
- `apc_engine`: DMC/MPC worker that reads PV/targets and writes MV setpoints. APC concept overview: [apc_engine/APC_ENGINE_OVERVIEW.md](apc_engine/APC_ENGINE_OVERVIEW.md)
- `controller_host`: Supervisor API that deploys model JSON and starts `apc_engine`.
- `hmi`: Rust backend + React frontend over WebSocket, including a built-in DMC model builder.

See [SYSTEM_OVERVIEW.md](SYSTEM_OVERVIEW.md) for data flow details.

## HMI Model Builder

- Build DMC models in-browser: define CV/MV/DV tags, weights, limits, and tuning.
- Choose model type: parametric (FOPDT) or step response (legacy import or generated from historian data).
- Import compatible third-party DMC model data for migration and reuse.
- Step Response Tool pulls historian trends, auto-detects steps, computes coefficients, and exports JSON for import.
- Visualize step response matrices, export controller JSON + OPC UA nodes YAML, or deploy directly to live services.
- Load existing controller models into the builder for review or edits.

## Quick Start (Local Dev)

### Prerequisites

- Rust (1.70+)
- Node.js (18+)

### 1) Configure OPC UA Endpoints

Update IP/host in:

- [opcua_server/config/serversettings.env](opcua_server/config/serversettings.env)
- [hmi/config/settings.toml](hmi/config/settings.toml)
- [virtual_plant/config/settings.toml](virtual_plant/config/settings.toml)

Details in [CONFIGURATION_GUIDE.md](CONFIGURATION_GUIDE.md).

If you build a portable bundle with `Build_scripts/windows/copy_portable.ps1`, run `portable-windows/CONFIGURE.ps1` to update IPs in the packaged configs.

### 2) Start Services (4 Terminals)

```bash
# Terminal 1: OPC UA server
cd opcua_server
cargo run
```

```bash
# Terminal 2: Simulator
cd virtual_plant
cargo run
```

```bash
# Terminal 3: Supervisor API
cd controller_host
cargo run
```

```bash
# Terminal 4: HMI backend (HTTP + WebSocket)
cd hmi
cargo run
```

Build the frontend before running the HMI:

```bash
cd hmi/frontend
npm install
npm run build
```

Then open http://localhost:3000.

For local development, HTTP is fine. For plant or production deployments, place HMI behind HTTPS/TLS (typically via reverse proxy) and expose `https://` / `wss://` only.


### 3) Start a Controller

- Use the HMI deploy flow to upload a model JSON, or
- Call the supervisor API to start a controller (it spawns `apc_engine`).

The engine uses CLI args for runtime config: `--model`, `--opc`, `--pki`.

## Models and Nodes

- Controller models are JSON files loaded by `apc_engine`.
- OPC UA node definitions are YAML files loaded by `opcua_server` and read by HMI.

See [apc_engine/src/config.rs](apc_engine/src/config.rs) and [apc_engine/src/modelloader.rs](apc_engine/src/modelloader.rs).

## Historian (Recommended but optional)

QuestDB logging is enabled in the HMI backend. Setup notes are in [hmi/QUESTDB_SETUP.md](hmi/QUESTDB_SETUP.md).

## Build and Test

```bash
just build-all
```

Organized platform-specific build and packaging entry points are available in `Build_scripts/`:

- `Build_scripts/linux/` for native Linux build/package wrappers.
- `Build_scripts/windows/` for native Windows PowerShell wrappers.
- `Build_scripts/linux_cross_compile/` for Linux -> Windows `.exe` cross-compile + packaging.

```bash
cd apc_engine
cargo test
```

## Docs

- [SYSTEM_OVERVIEW.md](SYSTEM_OVERVIEW.md)
- [CONFIGURATION_GUIDE.md](CONFIGURATION_GUIDE.md)
- [apc_engine/APC_ENGINE_OVERVIEW.md](apc_engine/APC_ENGINE_OVERVIEW.md)
- [apc_engine/DCS_ADAPTER_GUIDE.md](apc_engine/DCS_ADAPTER_GUIDE.md)
- [hmi/QUESTDB_SETUP.md](hmi/QUESTDB_SETUP.md)

## Safety Notice

This project is not certified for safety-critical use. Treat it as experimental software and validate thoroughly before any real-world deployment.

Operational guidance: validate all MPC/DMC outputs in your DCS/PLC. Use limit blocks, range checks, and rate-of-change limits on incoming setpoints, and never blindly trust any controller.

## Trademark and Affiliation Notice

This project is independent software and is not affiliated with, endorsed by, or sponsored by any DCS/PLC vendor.

Any third-party product or platform names are used only for interoperability/context. All third-party trademarks are the property of their respective owners.

## License

Apache-2.0. See [LICENSE](LICENSE).

Copyright (c) 2026 THR.
