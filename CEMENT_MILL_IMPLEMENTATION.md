# 水泥磨數位孿生與 MPC 專家工作區：實作與驗收

## 1. 本次範圍

使用者確認先完成水泥磨。原 React 為唯一產品前台，Runtime 與建模分開；真正 3D 設備、點位、三區趨勢與各任務功能整合。生料磨／旋窯／煤磨保留未來擴充，不在本輪開啟可操作入口。語言沿用 TypeScript／Rust／既有 Python 驗證模組與 C++ OPC DA 橋接，不增加語言。

本文件承接 `DIGITAL_TWIN_VALIDATION.md` 的逐頁規格，記錄實作位置、功能契約與直接驗收。設計 HTML 不算產品，短測試不替代三個月閉環。所有未列直接證據的項目均未驗收。

## 2. 本次開始時的基線與差距

- 前台目前仍由 `App.tsx` 在舊控制器畫面與 `Autoresearch` 五步流程間切換。`Autoresearch.tsx` 的研究草稿、資料與 run 選擇為元件內 state；換掉整個元件會丟失未保存狀態。
- `DigitalTwinScene.tsx` 與 `twinScene/` 已有 React Three Fiber 真 3D 原始碼，但 `DigitalTwin.tsx` 尚未接入；不可宣稱看板已可用。
- `types/EquipmentView.ts`、`validation/twin_equipment.py` 已有設備／設定版本契約；必須保留精確變數、座標、單位與不可變 session，不重建另一套。
- `research_twin.rs` 已有單一 native process owner 與序列命令，但目前只等候 HTTP 命令；須將連續排程由瀏覽器搬到 owner，並可關頁重連。
- 既有 CSV／PostgreSQL、清洗、研究、原生 DMC、封存趨勢、版本選擇與安裝模組可重用。各頁須接到真實功能，不只搬一個按鈕或展示假資料。
- HMI 啟用研究時禁止同時啟用現場 OPC／historian。這個隔離不因新版頁籤而移除；兩種部署能力需由伺服器明示。
- 本次開始時 `http://127.0.0.1:43180/` 連線遭拒，先前 `/tmp/authrity-campaign-tools.kti2k3/digital-twin-runtime` 不存在。舊程序／舊測試結果不能當新版本驗收證據。

## 3. 共用不變條件

1. Workspace 導航不改控制模式；工程草稿不寫現場。Runtime 設備／控制器選取與研究 run／session 選取隔離。
2. Runtime profile 擁有真實 Tag、單位、模式、硬限、每掃描 max_move 與 DCS 交接契約。研究候選只能引用，不得覆寫；映射變更需要新核准。
3. 同一物理 MV 只能有一個核准主控群組。Hold／退出不觸發其他群組自動搶占。初次 Engage、斷線／Hold 後恢復均重新明確授權。
4. OPC 接受、寄存器回讀、獨立 PID 應用回授分開。命令結果未知時查原操作，不重送；網頁被關閉不能丟失在途結果。
5. 研究或模擬的 CV／MV／DV、預測與動畫來自同一已確認來源／版本／sample。缺值、缺 DV、無預測不補零；過期不繼續顯示正常動畫。
6. CV、MV、DV 共用時間軸，各變數用獨立工程尺度；MV OP 不與 SP/PV 混軸。完整預測帶 issued time、valid time、horizon、模型與方法身分。
7. 收合只影響繪圖；保留選取／相機／曲線與草稿。背景 job 不依賴分頁時鐘。重連取最新確認快照，舊影格不補播。
8. 模擬 evaluation_plant 與控制模型獨立版本化。歷史預測驗證與閉環情境驗證分開；同源辨識模型不自稱工廠真值。
9. 自動搜尋不能更改評分、硬限制、plant 或切分來獲勝。Ts／模型記憶在 tuning 固定。保留每個 trial 與多目標折衷，保留集曝光後不得仍宣稱獨立驗證。
10. 可辨識性不足時不自動重辨識或對現場注入激勵。持續研究與候選發布為兩個不同授權流程。
11. 模型切換需用新模型與足夠品質合格的 MV／DV 歷史重建動態狀態，完成影子資格與首步限制檢查；不能把 PV 當穩態。DCS 接手未確認時禁止重新接管。
12. 每一操作入口要有實際 server capability、權限、已確認目標及失效回覆。缺少現場 commissioning 資訊可保存草稿，但不能假設 ABB 模式或放行現場控制。
13. 採集新鮮度與求解新鮮度分開。`acquired_at` 使用採集年齡限制；預測必須與原生 `observed_at`、`forecast_sequence`、instance、模型和時域一致，使用原生公布的 `maximum_solve_age_ms`。Ts=1s 的 producer 更新不延長 Ts=60s 的預測壽命。
14. 安全基線核對只解除目前操作聯鎖，不推定原未知操作成功或失敗。核對後的歷史未知結果不可被延遲回條或 GET 查詢改寫；發布排程也不能將它當作新基準。

## 4. 產品頁面與驗收矩陣

| ID | 必須實作的功能 | 直接驗收 |
| --- | --- | --- |
| 共用 | 水泥磨工作區；Runtime／建模隔離；固定來源／版本／品質／時鐘；每頁 3D 收合；手機與鍵盤 | 換頁／換模式不寫控制；草稿與選線保留；來源切換不短暫套用舊 Good 值；手機無整頁溢位 |
| R1 | 真 3D、設備選取、點位品質、控制摘要、三區趨勢、問題入口 | 只有具來源的部件動畫；選設備與訊號互相定位；未知值不扮正常 |
| R2 | 即時／歷史、各變數獨立軸、選線、完整預測、殘差、事件對時 | 同一時間像素對齊；不同量綱不相減；拖歷史不改現場控制 |
| R3 | 接管條件、受控 CV 目標／操作限、命令差異預覽、持久結果、正常退出與 DCS 握手 | 權限／CSRF／epoch／版本／品質／回授失效拒絕；未知結果可重查；無手動 MV 搶寫 |
| R4 | 事件與資料健康、時鐘／年齡／缺口、已閱讀與未解除狀態、操作時間線 | 斷線不顯示零告警；確認閱讀不清除保護；來源與事件可追溯 |
| R5 | 工況配對績效、每 CV 變異／偏差／越限、MV 動作／飽和、漂移與研究建議 | 不把 MV 動作當能耗；不相容窗口不合併排名；無有效樣本不可判定改善 |
| R6 | 候選證據、精確實例／profile 核准、安裝／啟動／接管分離、影子與安全交接、回退 | 作者不能自行核准；候選不能覆寫安全欄位；過期核准／profile mismatch 拒絕；無擾動與單一寫入者 |
| R7 | 連線能力與健康、授權範圍、稽核、保存／容量、維護變更 | 秘密不出現在前台；管理權不隱含操作權；維護不偷偷 reconnect 現場 |
| M1 | 研究階段、產物、真實任務進度、重新開啟、取消與下一步 | 刷新不重跑；失敗不當完成；取消顯示請求與確認狀態 |
| M2 | CSV、白名單 PostgreSQL、唯讀採集快照；來源／欄位／時區／品質預覽 | 原始 bytes 與來源保存；拒絕任意 SQL／DSN；匯入範圍及容量有界；不連寫入通道 |
| M3 | 設備配置、精確 CV／MV／DV 角色與點位、單位、時序、限制與回授、版本草稿 | 不猜 Tag／角色；修改不覆寫舊 session；缺失 mapping 保持缺失；慢化驗不能假裝即時 |
| M4 | 清洗前後比較、規則編輯、工況遮罩、缺值／離群／重複、時間切分、差異與 lineage | 不跨切分填補；規則只用允許訓練資料；預覽抽樣不截斷正式資料；保留集曝光追蹤 |
| M5 | FIR／FOPDT／SOPDT 的實際能力、動態辨識、激勵／共線診斷、模型資格、凍結模型 | 種類對應真實辨識器；完整 MV→CV／DV→CV；未知增益不為零；不能看 test 選模型 |
| M6 | 可搜尋欄位／範圍、固定目標、trial 差異、收斂、多目標折衷、有界搜尋與取消 | 固定 Ts／模型／plant；每個 trial 真實閉環；硬限不讓分數抵銷；基準可勝出 |
| M7 | 真 3D 模擬、選 trial／設定版本、單步／連續／暫停／重開、情境與三區完整預測 | 關頁後 server job 持續；暫停確認批次後停止；新 MV 造成新的 CV；129,600 掃描完整完成 |
| M8 | 基準／候選配對、代價、最差情境、留出資格、不可變候選送審 | 歷史預測與閉環證據分開；曝光後不重挑勝者；送審不安裝、不寫 MV |
| M9 | 新資料／漂移觸發、可辨識性、冷卻／去重／預算、停止條件與跳過原因 | 新資料觸發真工作；去重不重跑；缺資料不重辨識；暫停新研究不停止現場控制 |

## 5. 模組責任與整合方式

- `App.tsx` 與工作區容器擁有登入、模式／功能導航及已確認 Runtime 對象；不把研究選取提升成控制目標。
- `Autoresearch` 擁有 M1–M6／M8／M9 的持續草稿與產物選取，提供逐頁入口；重用 `ResearchImport`、`ResearchCleaning`、`ResearchStudy` 與結果／候選模組，不建立第二套研究資料格式。
- `DigitalTwin`／`DigitalTwinSession`／`DigitalTwinArchive` 擁有 M7 與封存觀察；共用 `DigitalTwinScene`、設備／設定編輯器、`ProcessTrendChart`。不讓 3D 自行查另一個來源。
- `hmi/src/research_twin.rs` 擁有 native session 與連續 job。命令序列化，GET snapshot 無推進副作用；start／pause 不能造成兩個 loop；中斷無有效 checkpoint 則明示中斷。
- `validation/research*.py` 與既有辨識／清洗模組擁有不可變資料與研究產物；HMI 的 research transport 繼續維持隔離及有界輸入／輸出。
- Runtime 後端擁有核准 profile、唯一寫入權威、durable operation registry、事件與健康證據；前台只提出帶目標與版本的原生操作請求。保留接受／回讀／獨立應用分離語意，移除任意 `writeOpcNode` 現場控制通路。
- `apc_engine` 擁有 DMC 計算、原生狀態、完整預測與安全門檻。3D／前台不得另外計算或假造 CV。
- `RuntimeHistory` 查詢 `/api/runtime/trends`；Tag 與精確來源由核准 profile 決定。目標／操作限使用 producer 獨立取樣的觀測節點；空窗口不冒充來源錯配，過去沒有封存的預測不回填。
- `RuntimeMaintenance` 只顯示實際連線回報、SQLite／檔案系統容量與配置限制；重連須由 Admin 加上全部群組的明確維護權限，並重新驗證安全交還。202 只代表已排隊，不代表連線成功。不新增自動刪除產物。
- `RuntimeVersions` 將核准、安裝、冷啟動 Monitor 與熱交接分開。冷候選啟動以投運 baseline hash／registry epoch 作前置條件，候選 hash 另列在 action；不得先要求接管舊模型。安裝意圖有效期不超過 60 秒，原生操作不超過 300 秒。
- `runtime_publisher.rs` 使用同一身分域、獨立授權、固定模型與允許調參範圍，只在既有健康 Engage 狀態執行已核准的版本交接；不得自行啟動、Arm 或事故後重新接管。
- 原生 `Status.forecast_issued_at` 只記錄成功編碼之同一預測的真實 UTC 時刻，與 `forecast_sequence` 一起更新；不是 OPC 寫入或 PID 應用回條。記錄新求解、fault 或模型 cutover 必須同時清除兩者，冷／不合格狀態為 null。發布器嚴格要求它等於 `observed_at`，另分別檢查求解與採集的 freshness budget；不得以缺值或接收時間替代。
- 發布觀測輪詢取全部投運 profile 中最短 `maximum_age_ms` 的一半，上限 5 s，為來源採樣邊界與排程抖動保留餘量；候選探索仍不早於每 30 s。這不放寬來源最大間隔、品質、獨立授權、冷卻或總派送預算。API 回報實際輪詢秒數，可含小數。
- 明確 `recover_monitor` 只恢復投運 profile 的原始 baseline digest，且必須在獨立 DCS release 確認後，於首次連線還原該模型的 CV weight／alpha 與 MV weight_r；不改 operator target、操作限或 SP。候選 cold start 仍依獨立核准；一般 start 不取得這項還原權限。`--recover-monitor` 不可搭配候選核准或 manual live tuning，durable start intent 亦固定此權限，重用 operation ID 不得增減。
- 未知結果驗證只中斷隔離工作區的真實引擎：先以 supervisor inventory、PID fd、執行檔、cwd、profile／private socket 及當前 instance／epoch／digest 核對所有權，再於 checked Exit 執行中中斷程序。producer 須明確以 `--acquisition-outage-secs 12` 啟用測試通路，SIGUSR1／SIGUSR2 只停發／恢復完整採集 frame；物理時鐘、輸入取樣、watchdog 與 twin 演算持續。單調時鐘自動到期，重複停發不延長期限，finally 仍要求提早恢復；恢復只發布新採樣，不回填時間。不使用 SIGSTOP、不放寬處理時限，不新增繞過安全停止的 HTTP 路由，也不改 journal 或假造 outcome。

共享 mutable 檔案只指定一位整合者：`App.tsx`、`Sidebar.tsx`、`hmi/src/main.rs`、`state.rs`、`config.rs` 由主整合者維護。其他模組先提供確定介面與呼叫需求，不各自改共用入口。

## 6. 驗證執行順序

1. 功能實作後一次執行相關前後端／native 回歸及建置；不把舊輸出當成新證據。
2. 在明確隔離的本機組態驗證 HTTP 登入／角色／CSRF、命令與 job 狀態；不得使用現場端點、既有工廠帳密或生產寫入。
3. 從 UI 匯入三個月合成水泥磨資料，清洗、辨識、資格、自動調參與候選比較；資料總量、split、模型與情境 hashes 可回查。
4. 由所選不可變 trial／設定版本建立新的完整季度 session，完成 129,600 個原生掃描。不得用減少樣本、回放或 mocked worker 代替。
5. 對 direct 與已配置的隔離 OPC DA 路徑分別核對命令、應用回授、CV 反應與完整 forecast。合成 plant 驗證不宣稱證明 ABB 工廠效果。
6. 關頁／重連／暫停／中斷、過期資料、失效 WebGL、來源切換、手機與 reduced motion；記錄實際 3D 繪圖／動畫與效能。
7. 只有前述完整測試通過後，才錄製從資料到閉環結果的原 React 操作影片；製作繁體中文字幕與中文配音，說明功能、實際資料來源、方法、各項參數如何設定與執行、績效及未經現場證明的限制。

### 測試通過後的影片驗收

- 完整演示 CSV／PostgreSQL／唯讀採集快照、CV／MV／DV 對應、清洗、FIR／FOPDT／SOPDT 辨識、MPC 自動調參、不可變候選與三個月閉環結果。
- 演示真正 3D 點位與動畫、三區獨立尺度趨勢、實際／預測／上下限選線，並說明如何判讀變異、偏差、越限與 MV 動作代價。
- 演示隔離 Runtime 的核准、啟動監測、Arm／Engage、版本交接、持續研究與有界發布、Hold／DCS 交接／安全退出。每個操作說明目標、前提、參數、確認結果與失敗時的處置。
- 使用實際操作錄影；等待段落可剪輯或加速，但必須標示，不以回放動畫或靜態原型冒充閉環運轉。章節可回查實際 run／session／operation ID 與驗證產物。
- 字幕須與實際畫面、數值、參數名稱及講解一致；配音使用中文，術語保持一致，不宣稱合成資料證明 ABB 工廠績效。
- 交付 MP4 操作影片（H.264 影像與中文音軌）、獨立 UTF-8 SRT 繁體中文字幕及章節說明，核對音軌、音量、字幕同步、關鍵畫面可讀性與完整流程；未完成配音或只有腳本不算影片交付。


## 7. 驗收證據與執行規則

驗證紀錄應保存命令／版本、輸出、run/session ID、資料與程式 hash、完整 trace／forecast 路徑、UI 操作與影片。各項僅在新版本直接證據存在後才標記通過。

終端操作授權以[專案協作規則](.github/copilot-instructions.md#collaboration-preferences-user-specific)為準：由代理自行執行已授權的本機準備、建置、測試與隔離服務，不再要求使用者代跑一般 shell 步驟。下方保留 Bash 與 fish 的可重現命令，依實際可用 shell 選用。若會話仍載入舊的禁止規則，需先重新載入專案指令；修改磁碟檔案不會自行解除該會話限制，也不得透過 Eval／子代理繞過。未取得實際執行結果的項目保持未驗收。

### 完整驗證入口

`validation/cement_workspace/workspace.py` 是驗證主入口，`Build_scripts/linux/cement-workspace.fish` 只是可選的包裝。主入口在**新的暫存根目錄**複製目前原始碼；不重用先前二進位，也不更新工作目錄的 lockfile。準備階段包含 React 建置、lint、全部前台行為測試、Rust 各 target 測試、Python 回歸、實際 COM fixture 與完整季度資料生成。所有結果仍須以當次輸出判定。

前置條件：可用的 Python 3.11+／NumPy、Rust、Node／npm、OpenSSL、make、MinGW／Wine，以及相容的既有 QuestDB runtime。完整資料來源驗收另外需要 PostgreSQL `initdb`／`pg_ctl`／`psql` 在 PATH。腳本不下載 QuestDB，也不默認重用任何既有資料庫。

以下的 `QUESTDB_JAR` 必須先指向真實且相容的本機檔案；也可將 `--questdb-jar "$QUESTDB_JAR"` 換成 `--questdb-java` 加上既有 QuestDB bundled Java 的完整路徑。

**沒有已知 QuestDB runtime 時（Bash、Linux x86-64／WSL2）：**

使用官方固定版本 `10.1.0` 的 runtime 套件；它自帶 Java 與 `io.questdb` 模組，因此改用 `--questdb-java`，不需要設定 `QUESTDB_JAR` 或替換系統 Java。套件 URL 與 SHA-256 取自 [官方 release metadata](https://api.github.com/repos/questdb/questdb/releases/tags/10.1.0)，解壓後的 `bin/java` 與模組入口依據 [官方部署文件](https://questdb.com/docs/deployment/systemd/)。此處不安裝 systemd 服務、不使用 sudo，也不沿用既有資料庫；相容性仍須以本輪完整執行結果驗收。

以下命令明確允許下載官方 QuestDB 套件與 Cargo／npm／Python 依賴；系統編譯工具仍須已安裝。所有命令包在 Bash 子 shell，任一步失敗即停止後續步驟，不會關閉目前終端。runtime 解壓到新的使用者目錄，資料庫由驗證入口建立在新的 `/tmp/cement-*` 根目錄。

```bash
(
  set -euo pipefail
  cd /home/timmypai/apps/AuTHRity-APC
  umask 077
  mkdir -p "$HOME/.local/share/authrity"
  qdb_home="$(mktemp -d "$HOME/.local/share/authrity/questdb-10.1.0.XXXXXX")"
  archive="$qdb_home/questdb.tar.gz"
  printf 'QuestDB runtime：%s\n' "$qdb_home"

  curl --fail --location --proto '=https' --proto-redir '=https' \
    'https://github.com/questdb/questdb/releases/download/10.1.0/questdb-10.1.0-rt-linux-x86-64.tar.gz' \
    --output "$archive"
  printf '%s  %s\n' \
    '567994861a032bd939588ed207e4f074db975c74ea5386cd2e01c8d68136b244' \
    "$archive" | sha256sum --check -
  tar -xzf "$archive" -C "$qdb_home" --strip-components=1 --no-same-owner
  env -u JAVA_TOOL_OPTIONS -u JDK_JAVA_OPTIONS -u _JAVA_OPTIONS \
    "$qdb_home/bin/java" --describe-module io.questdb

  root="/tmp/cement-$(date +%s)"
  printf '驗證根目錄：%s\n' "$root"
  python3 -B validation/cement_workspace/workspace.py prepare \
    --root "$root" --questdb-java "$qdb_home/bin/java" \
    --postgres --allow-dependency-downloads
  python3 -B validation/cement_workspace/workspace.py start --root "$root"
)
```

成功執行 Java 模組檢查只代表 runtime 前置條件可用，不代表 MPC 或閉環已通過。`prepare`、`start` 與後續 `verify` 保留原有 PostgreSQL、實際 OPC DA COM、全季度資料及失效恢復驗收；不得以直接模式或停用 historian 代替。

**目前終端是 Bash 時：** 不使用 fish 的 `set root ...`、`(date ...)` 或 `and`。下列命令先核對 QuestDB 路徑；準備失敗不會繼續啟動。

```bash
cd /home/timmypai/apps/AuTHRity-APC
if [ -z "${QUESTDB_JAR:-}" ] || [ ! -f "$QUESTDB_JAR" ]; then
  printf '%s\n' '請先將 QUESTDB_JAR 設為實際相容的 QuestDB JAR 檔案路徑。'
else
  root="/tmp/cement-$(date +%s)"
  printf '驗證根目錄：%s\n' "$root"
  python3 -B validation/cement_workspace/workspace.py prepare \
    --root "$root" --questdb-jar "$QUESTDB_JAR" --postgres &&
  python3 -B validation/cement_workspace/workspace.py start --root "$root"
fi
```

**已進入且安裝 fish 時，才使用：**

```fish
set root /tmp/cement-(date +%s)
fish Build_scripts/linux/cement-workspace.fish prepare --root "$root" --questdb-jar "$QUESTDB_JAR" --postgres
and fish Build_scripts/linux/cement-workspace.fish start --root "$root"
```

預設僅使用離線依賴快取。若明確允許下載 Cargo／npm／Python 依賴，在 `prepare` 加上 `--allow-dependency-downloads`；不因此減少驗收案例。保留 `start` 的前景程序，在另一個終端執行啟動輸出中的 `FULL API ACCEPTANCE` 命令；該命令使用準備好的 Python 與原始碼快照，已包含實際 root，不依賴 fish。也可自行帶入同一個實際 root：

```bash
python3 -B validation/cement_workspace/workspace.py verify --root "$root" --acknowledge-owned-faults
```

第二個終端必須自行設定成第一個終端印出的實際 root；不能重新產生日期根目錄。`--acknowledge-owned-faults` 明確允許對已驗明身分且啟用測試通路的 simulator 執行有時限的採集停發／恢復，以及中斷該 workspace 的 loopback 引擎，用於真實未知結果與安全基線恢復測試。不暫停物理程序；腳本不得對現場程序執行這些動作。

若自有工作區上次中斷留下原生 authority socket，先核對原操作回條、確認原程序已退出且 socket 不可連線，再以 `start --root "$root" --recover-monitor` 明確恢復原始 Monitor。此選項不授權 Engage、不載入候選、不刪除舊操作紀錄；原生可連線檢查與 writer-lock 防重複寫入仍有效。啟動 intent 與 HTTP 拒絕內容保留於工作區，移除帳密後才寫入診斷。

`async-opcua 0.17` 的測試信任庫使用平面 `trusted` 目錄，檔名須符合函式庫的 CN／SHA-1 指紋格式；不是 `trusted/certs`。僅預置信任此工作區生成的伺服器憑證，不以自動信任未知憑證解除 replay 的安全檢查。

Historian 使用來源時間，不以接收時間補值。HMI 的 250 ms 採樣／1 s 發布需保留多筆通知；原先 queue size 1 曾產生 Overflow 與同時間 ReadAll Good 衝突，R5 正確拒絕該時窗。佇列改為 8 後，獨立 historian table 與重新啟動的主 Runtime 都經原 React R5 驗證：兩個 60 s 時窗各取得 60 個完整樣本、100% 覆蓋率、零壞樣本／缺口。主 Runtime 證據為 `ui/main-historian-queue8-evidence.json`。舊衝突資料與拒絕規則保留；兩個時窗均為 Monitor／定常合成製程，只證明資料恢復，不代表 MPC 改善。

需由 UI 選版時，先以 `--until-selection` 停在選版邊界；完成原 React 操作後，以 `--resume-run` 加上實際 run ID 接續，仍須跑完所有驗收。自動 API 驗證不取代後續瀏覽器桌面／手機、3D、完整操作與錄影驗收。

研究與 Runtime 是不同 listener，使用同一個隔離身分資料庫及不同 session cookie。候選作者與核准者必須不同；測試密碼 `admin` 僅用於明確本機測試組態與新的隔離帳號，不覆寫既有帳號。
本機測試 listener 才允許 HTTP session cookie；其他部署維持 Secure cookie。測試模式拒絕載入 `field` commissioning profile，`synthetic_local` profile 另須使用明確 loopback IP；不得透過測試密碼啟動現場設定。

### 目前狀態

- 設計與逐項驗收矩陣、M1–M9、R1–R7 及完整隔離驗證入口已建立。
- 實際驗證工作區為 `/home/timmypai/cement-v4`；Rust、Wine、MinGW、PostgreSQL 與官方 QuestDB 均使用使用者目錄／自有 loopback 服務，不接觸現場。
- 前端建置通過；lint 無錯誤，仍有 `ProcessTrendChart` 的兩項既有 hook 相依警告。前端 55、APC 157、supervisor 15、HMI 77、Python 187 項測試通過；Python 原生整合案例不再因未指定 binary 而跳過。Python 尚有 SQLite connection ResourceWarning。原生 ready 後清除暖機訊息、但保留 latched Hold 原因的回歸案例已確認修正前失敗／修正後通過；新程序以真實 Ts=60 s 累積 50/49 筆歷史後，實測 healthy=true、detail 清空、Monitor 未解鎖。完整 forecast 的 issue time、sequence、instance、model 與原生狀態一致；見 `ui/corrected-native-warmup-ready.json`、`ui/corrected-native-forecast-identity.json`。
- 已修正實際建置／測試發現的前端型別與狀態生命週期、forecast/native 時戳一致性、HMI 操作 schema／安裝授權，以及 immutable equipment view 深複製。各命令原始輸出留在工作區 `commands/`。
- 真實 OPC DA COM fixture 已執行；完整 129,600 scan 季度輸入已生成。原 React 已完成 CSV、唯讀 PostgreSQL、含原始 SHA-256 manifest 的完整季度 history snapshot 匯入及清洗；清洗後的工程契約保留六個訊號的確切來源欄位與品質欄位。Snapshot 明確標為 synthetic native replay，不冒充工廠採集。
- 真實 OPC UA replay 曾因信任庫路徑錯誤而拒絕連線；修正預置信任後，已觀察到連續成功的加密讀寫。啟動請求已補齊操作 ID／期限；Wine 無剩餘程序時的清理狀態不再掩蓋真正啟動錯誤。
- 原 Runtime WebGL 已驗證設備選取、收合／重開相機與像素保持、動畫／reduced-motion、斷線保留最後值並停止動畫、重連恢復、實際 context loss 與重試，以及換頁不送控制寫入。Chromium iPhone 13 模擬另驗證 390 px 無水平溢位、實際 WebGL、觸控旋轉／雙指縮放／點選與鍵盤操作；不是實體 iOS 驗證。證據位於 `ui/phase1-evidence.json`、`ui/phase2-evidence.json`、`ui/mobile-webgl-evidence.json`。
- 原 React M5 的 FIR、FOPDT、SOPDT 已各取得不可變模型與實際資格回條；均為歷史預測資格，不具 factory authority，不宣稱未接觸的 holdout 或因果改善。見 `ui/model-family-qualification-evidence.json`。
- 季度 run `49a424ad96e64eaf945bc549fec3930e` 完成 26 筆開發試驗後，在 80/94 工作單元停於 `awaiting_selection`。原 React M6 依已通過門檻的開發分數及非支配集合，於 2026-10-29 05:28:20 UTC 明確凍結 `standard_dmc-trial-011`、`soft_band-trial-024` 與理由，才啟動最終留出驗證。06:23:18 UTC 已完成 94/94；兩方法的 nominal／gain_low／gain_high、完整掃描及真實 COM 等價共 16 項檢查通過，均為 `candidate_for_review`。來源有 cleaning preview 曝光，holdout 仍為 exploratory／independent=false，不能稱未接觸留出或工廠資格。見 `ui/manual-presealed-selection-evidence.json`、`ui/completed-sealed-research.json`。
- 原 React M7 已開啟 `standard_dmc-trial-011` 與基準 `standard_dmc-trial-000` 的 40,320 掃描開發封存 A/B，切換來源無寫入；這不是完整季度或留出資格。實測發現影格摘要誤印來源起點 UTC，已改用既有游標時間；修正前第 1 筆錯顯 00:00，修正後為 2026-02-01 00:01 UTC，末筆 40,319 為 2026-02-28 23:59 UTC 且下一筆禁用。缺少該次完整 forecast 時明示未封存，不以單步預測／目前模型回填。前端 55 項與建置通過；回歸採原 UI 非零掃描與末筆直接檢查，未新增綁定文案的測試。見 `ui/recorded-trial-ab-evidence.json`、`ui/archive-timestamp-failing-before.json`、`ui/archive-timestamp-passing-after.json`、`ui/archive-time-corrected.webp`。
- 原 UI 的完整季度 Session 曾因 HMI JSON 解析改變目標值 1 ULP 而中斷；原生 trace 與凍結情境本身一致。HMI 已啟用 `serde_json/float_roundtrip`，保留 Python 的嚴格目標相等檢查；正式回歸修正前失敗、修正後全部 77 項 HMI 測試通過。研究完成後才切換 HMI，14 個凍結 Python 來源及 campaign binary 雜湊不變。新原 UI Session `0ec693b48d6c494aa65c25775bbd73dc` 已完成 129,600 掃描／求解，零 solver failure，129,600 組目標逐一與情境精確相等，完整 forecast 索引亦有 129,600 筆。原故障 sample 17,408 經實際 HTTP 取得的兩個目標位元值保持一致。見 `ui/hmi-precision-installed.json`、`ui/original-ui-quarter-integrity.json`、`ui/native-target-actual-frame-green.json`；舊失敗 Session 不覆寫或冒充恢復。
- HTTP 原生 Session 已完成兩方法各 direct／真實 COM 的四組完整 129,600 掃描，均驗證單步、冪等、衝突 ID 拒絕、唯讀快照、關頁續跑、批次停止、恢復及完整 forecast／archive。兩方法的時間、CV／MV／DV、targets、command、prediction、quality、solver status 在 direct／COM 間差異均為零；兩個 MV 的配對干預亦各跑完整季度並量得 CV 響應，不宣稱工廠因果。證據為 `acceptance-1793256194483323788/manifest.json`；該次整體 attempt 仍標為 failed，因後續 Runtime 維護拒絕碼契約比對中止，不能改寫成全程通過。
- 原 M7 的背景快照讀取曾阻擋已顯示可用的控制按鈕，明示「尚未送出」而未產生命令。現已讓既有 busy 狀態同步反映讀取聯鎖，不排隊或自動重送。實際 Chromium 延遲真實 GET 1.5 s 時，快照／單步／啟動／關閉按鈕禁用；真實回覆後恢復，無 POST。55 項前端測試、lint／建置通過，新 bundle 原子切換而未重啟服務。原 UI 在 51,201 掃描確認暫停，跨頁後掃描數不變；明確恢復後真正關閉分頁，再開啟時已到 113,153，operation ID 不變，最終完整完成。見 `ui/session-read-gate-browser-smoke.json`、`ui/corrected-pause-stable-across-navigation.json`、`ui/native-closed-tab-progress-evidence.json`。
- 原 M7 已另存設備 v2 `8c95079d2bc9c576d70f62f3a711b3c0a06ba37f0ba2d7f61039c109439de925`；示意 separator 的 x 由 8 改為 9，點位綁定不變。執行中的 Session 仍保存 v1 `205aba42fb0735890f93959aaaa92c543babb6c9e82c5fa11b73e7732be56578` 與 x=8。示意幾何不作工廠配置證明，未建模的 OP／PID modes 保持缺值；見 `ui/equipment-v2-immutable-session-evidence.json`。
- 原 React M8 已將 standard DMC 保存為精確 profile 的不可變送審版本 `d1a8e85fbcfd367bb261bda9df4ce16d2cacd5f92fb3bb2f57db39b461a2caf1`，authority 為 `review_only`，保留 exploratory holdout。操作前後 Runtime 的 instance、baseline model、Monitor 與 armed=false 均不變；沒有安裝、啟動或接管。見 `ui/original-ui-review-only-candidate.json`。送審仍須另經獨立 Runtime 核准及後續操作。
- 原 React R2 已查詢精確來源的 2 分鐘歷史，取得 2,400 筆原始／顯示點；不以目前 forecast 回填歷史。實測工程單位獨立軸、相同時間繪圖邊界、選線／限制帶跨頁保留、鍵盤與真實指標拖曳共用游標，以及 30–90 s 檢視縮放；無控制寫入。見 `ui/runtime-history-source-evidence.json`、`ui/runtime-history-controls-evidence.json`、`ui/runtime-history-cursor-evidence.json`。
- 原 React R3 已驗證超出硬上限的 target=500 被瀏覽器 range validation 阻擋、取消預覽不寫入；合法目標 360→361→360 與操作上限 390→385→390 都取得不同 operation ID、原生 accepted／confirmed 回條及 producer 獨立取樣值。工程硬上限維持 390，原生維持未解鎖 Monitor、applied SP 不變；查詢原操作未重送。見 `ui/runtime-cv-target-operation-evidence.json`、`ui/runtime-operating-limit-evidence.json`。
- 原 React R3 已實測未 Arm 的 Engage 被原生拒絕，再經明確 Arm／Engage 取得兩個 PID 的 requested／actual RCAS=3，以及不同來源時間的 SP 命令與獨立 applied SP。跨數個 Ts 的真實 MV 移動與 CV 量測、控制中換頁零寫入均留有證據。Hold 跨越完整掃描後不再改寫 SP；此實作另執行 checked release，只有取得獨立 DCS ACK 與 actual local PID modes 才報告交還。已另行 Exit 並回到未解鎖 Monitor；見 `ui/runtime-ui-arm-engage-evidence.json`、`ui/engaged-navigation-evidence.json`、`ui/runtime-ui-hold-evidence.json`、`ui/runtime-ui-exit-monitor-evidence.json`。
- 此流程發現前端錯誤地要求冷 Monitor 先完成暖機，與 HMI／原生契約不符。共享 readiness gate 已分離 Monitor 恢復與 Arm／Engage 的健康要求；未知操作聯鎖、權限、instance／epoch、確認與原生再驗證均保留。回歸測試修正前失敗，修正後全部 55 項前端測試通過；在同一原生 instance、history=17/49、healthy=false 下，原 React 已成功確認 Monitor，Arm／Engage 仍不可用。新靜態 bundle 原子切換，未重啟服務或修改執行中研究的 Python／campaign binary。見 `ui/cold-monitor-ui-failing-before.json`、`ui/cold-monitor-ui-passing-after.json`、`ui/frontend-monitor-cutover.json`。
- R5 另實測兩個各有 60 個完整有效樣本、但 target 分別為 360／361 的時窗不能比較；查詢先前實際採集停頓的完整 60 s 時窗，取得零樣本、coverage=0、qualified=false，變異／RMSE 為 null，而非零或改善。未再次注入故障。見 `ui/runtime-incompatible-target-windows.json`、`ui/runtime-missing-acquisition-window.json`。
- 原 React R7 已驗證 Admin 的唯讀維護查詢不要求重連；獨立 reviewer 雖具 Admin 角色，缺少 profile 的 `runtime.maintenance` 時，UI 禁用查詢／不提供重連，後端回傳 403。見 `ui/maintenance-readonly-evidence.json`、`ui/reviewer-maintenance-scope.json`。
- 原 R4 已在先前真實採集過期期間標記事件 8 已讀；HTTP 回條為 `interlocks_changed=false`，隨後來源條件仍 active、原生仍 Hold、最後已知 SeparatorSpeed=900 保留且標示 stale。這只證明已讀不解除保護；該次 90 s 故障的恢復失敗仍依下項保留。見 `ui/active-event-fault-evidence.json`。
- 手動暫停 producer 90 s 曾超過 OPC UA session 存活時間；恢復後得到 `BadSessionIdInvalid`，工作區依規則停止全部子程序。當時 run `94ab4d659e8a4bf495ca1a768cd8a499` 在 67/94 工作單元中斷，保留失敗，不計通過。已確認舊引擎退出／authority socket 不可連線，以 `--recover-monitor` 重新暖機，並由原 React 啟動新 run `49a424ad96e64eaf945bc549fec3930e`。後續只能使用不停止物理時鐘的 bounded publication outage／finally resume 協定，且不得在研究執行中注入來源故障。
- 原 M9 已啟用最多一個 run 的固定模型政策，真實新 seed 季度清理資料 `ds_db6bbcef5ff544e3a1ea2de1fb3ec620` 觸發 run `5b3069ac6444b7e93af6e2dc624e3fc0`。原 UI 暫停政策後，既有研究仍繼續；再於 M1 請求取消，先收到 `cancel_requested`，後由伺服器確認 `cancelled`，保留 31/95 工作單元與 9 個完成試驗，不計完整季度通過。重新啟用後，排程器實際記錄 `budget_exhausted`；取消未退還 run=1／trial=26 的預留預算。見 `ui/continuous-research-pause-preserves-work.json`、`ui/policy-run-original-ui-cancel-confirmed.json`、`ui/policy-budget-exhaustion-original-scheduler.json`。
- M1 曾在伺服器確認取消後仍保留「等待確認」通知；已移除黏著式通知，改依所選伺服器 run 狀態及 ID 顯示。原 UI 現正確顯示取消確認，選取另一個已完成 run 後不再帶出舊取消通知；見 `ui/cancel-confirmed-stale-notice.json`、`ui/cancelled-run-server-derived-original-ui.json`、`ui/cancellation-notice-cleared-on-run-switch.json`。55 項前端測試、lint／建置通過；新靜態 bundle 原子切換，未重啟服務。
- M9 保存新版本會在伺服器暫停舊版本；原 UI 曾留下舊 enabled／revision，後續操作被正確以 422 拒絕。現沿用既有清單重讀機制，保存後重新取得全部版本。原 UI 實測 c2e9… 從 enabled/revision 2 被新版本取代後，自動顯示 paused/revision 3；新政策 `policy_779a9ad4348344bcb6388548f62c5529` 亦保持 paused，預算 2 runs／52 trials，未送任何 Runtime 寫入。見 `ui/policy-supersession-server-refresh-smoke.json`。該中間版本未執行；來源去重由下述獨立漂移政策驗證。
- 舊錯誤 immutable cleaning 記錄仍保留；政策實際記錄工程欄位不匹配而跳過，未將其當合格新資料。原 React 以 PostgreSQL 觀測歷史建立自動重辨識政策的請求，也由後端以 422 拒絕未經伺服器核實的開環／因果資格；見 `ui/observational-reidentification-denial.json`。
- M9 持續漂移／冷卻／來源去重已由原 React 與正常 30 s 排程器實測：事先固定 NRMSE=0.05、兩個非重疊 validation 時窗及最少 120 s，冷卻 300 s、預算 2 runs／52 trials。相對時間來源被 `source_interval_unknown` 拒絕；第一個絕對時間視窗超過門檻但仍被 `drift_not_sustained_across_qualified_windows` 拒絕；第二個視窗實際觸發 `6298d604470639a092abbba5fe9b4e97`。該 run 在 2/95 因凍結模型 free-run NRMSE [0.1095855, 0.0798182] 不合資格而失敗，保留失敗、不產生候選、不計完整研究通過，也未放寬資格門檻。正常排程器隨後記錄 `cooldown`；真實 300 s 過後，不同 dataset ID、相同 source/history SHA 的新清理產物被 `duplicate_source_claim` 拒絕，預算仍 1/2 runs、26/52 trials。原 UI 最後暫停政策，Runtime instance／epoch／digest／authority 全程不變。見 `ui/drift-policy-acceptance-manifest.json`、`ui/drift-dedup-original-policy.png`。兩個來源是預先宣告的新 seed 730203／730204、全部 path gain ×1.5 的原生 129,600 筆仿真，UTC 起點 2026-04-01／2026-07-01，各 90 天，不冒充現場採集或三個日曆月；來源與事先規格在 `m9-drift-fixtures-1793271144990064151/`。
- Runtime API 續跑 `acceptance-1793267172778895199/` 已通過 Engage 中的維護拒絕與 scope 拒絕，並確認切換 soft-band、回復 DMC 的精確 digest；維護拒絕後 connection generation、instance、model、authority 不變。另完成發布 template run `94826338cc044fcc80771df2c0867587` 的完整季度、26 trials、獨立核准／安裝及原生切換。發布政策作者自核被正確以 409 拒絕，但驗證器仍預期 400/403 而中止；該失敗保留。驗證器現嚴格接受 409，並檢查政策 revision、paused、空 grantor 不變；實際 API smoke 亦證明 instance、epoch、model、authority 不變，見 `ui/publication-author-denial-409-smoke.json`。當時的自動發布、故障恢復與重連尚未完成；後續續跑與最終結果見下列紀錄，未放寬安全閘門或重做該次已完成季度。
- 原 React R6 已完成獨立核准、安裝、冷 Monitor、真實暖機、Arm／Engage、不同方法交接與回復。reviewer（actor 2，作者為 actor 1）核准原 M8 候選 `d1a8e85fbcfd367bb261bda9df4ce16d2cacd5f92fb3bb2f57db39b461a2caf1`；核准與安裝均未改變原生 authority。原 R3 確認 Exit 的獨立 DCS handoff 並停止舊引擎；R6 啟動精確 digest `4afb4310…` 的新實例 `ba1bf49e-2d66-44d0-8082-d12bb270756b`。冷 Monitor 最初 healthy=false、armed=false，UI 禁用 Arm／Engage；只讀觀察 2,788.916 s 後取得 49/49 真實歷史樣本。明確 Arm 進入 `armed`（不是 Monitor 或 Engage），DCS handoff 與 applied-SP 保持不變；另行 Engage 才接管。之後原 R6 成功切換 soft-band `3169613e…` 並回復 DMC `4afb4310…`，同一原生 instance，所有操作均有 accepted 回條。完整索引為 `ui/r6-original-workflow-acceptance.json`。
- 發布來源續跑保留既有合格 template，只另建一個在 M9 fixtures 匯入後建立的來源政策，避免其他驗證資料被排程器誤選；數值發布界限不變。`publisher-continuation-1793278305450705227/` 已完成來源政策 run `4af132f7bb06df98b0f7a62bf3654450`：兩種方法、六輪、26 個使用驗證切分的開發試驗；凍結候選後的季度評估為 129,600 scans，16 項檢查通過。開發試驗的 40,320 scans 不混稱完整季度。實際自動封存候選為 `764d7c2b…`，model `4e7cef90…`。該來源研究沒有發出 Runtime 操作。seed 730202 曾在先前 M9 暴露，不能宣稱未觸及的獨立 holdout；完整來源證據為 `source-research-manifest.json`。
- 真實發布曾被 `Native forecast is not bound to the last qualified solve` 阻擋：舊 native 狀態缺少 `forecast_issued_at`。已在成功編碼預測的邊界傳入同一 issued-at，並在新求解／fault／cutover 清除；37 項 native 測試與 release 建置通過（`commands/0119-*`、`0120-*`）。安全交還、停止並冷啟動新 binary `d6cf6b64…` 後，冷狀態先如實回報 null；真實暖機後，端點以 `2026-10-29T16:25:19.316759782Z` 證明 forecast 與 solve 時間相同，見 `runtime-continuation/repaired-native-live-smoke.json`。原 `mpc_campaign` 仍為 `814f6849…`；完整研究與 COM 證據未重跑或改寫。
- 下一個真實發布 job `e6ee3806…` 已排入，但在派送前因觀測窗口失效而拒絕；原生 operation 查詢 404、已用預算 0、原 template digest／instance／epoch／Engage 均未改變。原排程每 5 s 輪詢，恰好等於 profile 的 5 s 來源間隔上限；實際唯讀來源量測含 5,001 ms 間隔。確定性回歸先重現合格窗口無法維持，再改由投運 freshness budget 決定較短輪詢，保留真實來源缺口拒絕；HMI 78 項測試及建置通過（`commands/0121-*` 至 `0123-*`）。新執行檔 `5beffbf0…` 的實際端點回報 2.5 s；候選探索仍維持至少 30 s，未增加任何界限或預算。
- 舊拒絕 job 永久保留，舊發布政策已撤銷，不重排、不重送。僅在證明沒有派送後，建立規格完全相同、仍限一次派送的新明確意圖 `cement-cadence-95fc88f5535f43e79f56aed37f6ffa13`；原 React R6 的 reviewer 獨立授權第 2 版，未改變冷 Monitor 的 instance／epoch／digest／authority。真實暖機及明確 Arm／Engage 後，job `b2cf5d84…`／operation `publisher_b2cf5d84…` 已接受，原生保持同一 instance，確認精確候選 model `4e7cef90…` 與獨立 applied SP；一次預算已用盡，剩餘 0。政策已暫停至第 3 版；核准回復 template、Exit 的獨立 DCS release 與 Stop 均有 accepted 回條。見 `publisher-continuation-1793278305450705227/cadence-repair/runtime-acceptance/actual-native-publication.json`。
- 後續原 baseline RecoverMonitor 曾因 OPC 仍保留候選 tuning 而進入 disarmed Hold，並非自動接管；原 `f89b19e9…` 只確認程序建立，未證明 Monitor readiness。失敗 manifest 與原生凍結調參拒絕均保留。已修正明確 recovery 權限及一次性調參還原；CLI 邊界與 durable intent 權限變更測試先紅後綠，38 項 native／16 項 supervisor 測試及兩個 release 建置通過（`commands/0126-*` 至 `0131-*`）。
- 新 native `07ec716e…`／supervisor `2a07afec…` 已實際運行。使用原核准 template 冷啟動，再在不重啟 OPC／producer、不重新 provision 的情況下停止候選並執行新 recovery `7b6ef992…`：原 baseline `f7fd1da2…` 的新 instance `8e4559d9-0348-408a-bda1-78ced47bf275` 取得真實採集，Monitor、armed=false、safe_baseline=true、DCS handoff=true；全部 operator target／limit／SP 前後相同。隨後以原 Ts=60 s 完成 49/49 真實歷史，healthy=true，forecast／solve issued-at 同為 `2026-10-29T19:04:05.221531042Z`。見同目錄 `baseline-recovery-repair/restored-baseline-live-smoke.json`、`frozen-tuning-selection-evidence.json` 與 `remaining-acceptance/restored-baseline-warmed.json`。
- 後續實際中斷產生 `cement-fault-exit-34a6358c…` 的 unknown／blocking 與 active stale event 65；已讀沒有解除來源條件。但整個 producer 被 SIGSTOP 約 6 s 後，固定物理時鐘正確以 `OPC processing exceeded sample_time; refusing accelerated catch-up` 終止，工作區依規則退出。該失敗不改寫。問題在注入方式，不放寬物理時鐘；新的隔離 simulator 通路僅停發完整採集，4 項邊界測試及 release 建置通過（`commands/0139-*`、`0140-*`），14 個科學 Python 來源與凍結 campaign binary 不變。
- 新工作區以明確 baseline recovery 建立獨立安全 Monitor，clearance `cement-post-shutdown-clearance-43b51741…` 只解除原未知操作聯鎖，歷史仍為 unknown；舊 stale event 的 read_at 保留，由真實新來源另行清除。這是新 source／native 的安全基線，不冒充延續已停止的物理歷史。再實測 SIGUSR1 停發並重複 SIGUSR1，未送 SIGUSR2，13.562 s 內取得同一 producer 的新鮮完整 frame，證明自動期限未被延長；包含 tick／觀測延遲，並非把 12 s 配置宣稱為零延遲。見 `bounded-acquisition-acceptance/actual-automatic-outage-expiry.json`。
- 該資料缺口後，完整 49 樣本雖恢復健康，原生仍依 `DiscontinuousScan` 保護鎖在 Hold，要求再次明確 Monitor；續跑腳本原先要求自動維持 Monitor 的斷言不成立，該失敗保留。後續使用新的明確意圖，不重送舊命令、不繞過 Hold。
- 最後續跑 `baseline-recovery-repair/qualified-recovery-acceptance/manifest.json` 已通過：真實有界停發加原生中斷，原操作 `cement-fault-exit-8ed9ac5c…` 仍為 unknown；獨立 clearance `cement-clearance-af91029f…` 只解除目前聯鎖。同一 producer 的新鮮完整 frame 確認恢復，active stale event 75 的已讀與解除時間分開。新原 baseline instance `a90f3e1d-7847-43bc-b50e-5ce4f97aee8f` 為未解鎖安全 Monitor；實際 OPC 重連 generation 1→2，取得更新來源時間，native instance／digest／Monitor／DCS handoff 不變。202 queued 沒有被當成 connected。
- M1–M9、R1–R7、完整季度 direct／真實 Wine COM DA、實際有界發布及故障恢復／重連的隔離軟體驗收均已完成。`/home/timmypai/cement-v4/acceptance-checkpoint.json` 保存 17 個矩陣項目的具雜湊證據索引；`video/acceptance-gate.json` 固定錄影前的通過證據，原索引與舊 failed／partial attempts 不改寫。原 React 影片已交付於 `video/delivery/cement-closed-loop.mp4`：12 分 08.3 秒、1080p H.264、中文 AAC、16 章及 121 段繁體字幕；同目錄提供獨立 SRT 與 `GUIDE.zh-TW.md`。全檔 21,849 影格解碼、音量／字幕時碼及 Chromium 全 16 章定位、實播已通過；`acceptance-final.json` 彙整軟體、媒體與最終安全狀態。不宣稱工廠資格、實體 iOS 或真人聽審；語音來源限本機驗證，未公開發布。
- 錄影另由原 M7 新建 Session `90eb8cdabf7e4ef3a4740fd90a2feba1`，真正完成 129,600 scans／solver calls、零 solver failure；逐列核對情境時間／目標、串流 trace 與完整 forecast，見 `video/operations/full-quarter-integrity.json`。原 Runtime 另完成獨立核准、冷 Monitor 的真實 49 筆暖機、Arm／Engage、360→361→360、同一 instance 的核准版本交接及精確回退；Hold 的 66.5 秒觀察確認無新 SP 且獨立 DCS／local 回授新鮮。Exit／Monitor 後如實重新暖機，重連實際取得 generation 3；製作期間自然恢復 healthy、未解鎖 Monitor，沒有自動接管。操作回條與最終唯讀觀察均保存於 `video/operations/`。
