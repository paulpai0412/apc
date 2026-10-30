# 歷史資料驅動的 MPC 驗證與優化設計

更新：2026-10-26。下方具日期的驗收段落只代表當時的程式與產物；第 3–5 輪新增程式尚待本輪建置、整合與實際 UI 驗收，不能沿用舊測試數量宣告通過。

## 目標與證據範圍

先以可重現的三個月合成資料驗證完整軟體閉迴路，再將同一資料契約與驗證程序套用到經准入的真實工廠歷史。**合成資料結果不證明真實水泥磨符合性；不能先找最低模擬分數再宣稱工廠吻合。**

歷史回放本身不會對新的 MV 作出反應。模擬器必須先有從資料辨識且經留出資料驗證的動態模型，才能回答「若改動 MV，CV 可能如何變化」。這是帶條件的反事實預測，不是從歷史資料直接查表得到真值。

| 使用者需求 | 實作方向 | 驗收限制 |
|---|---|---|
| 歷史資料供應模擬器與 OPC DA Server | 獨立動態工廠、三個月場景、原生 Windows COM DA Server 與 JSONL client bridge | Wine 本機 COM 不等於網路 DCOM 或 ABB 實機驗收 |
| 定義 CV/MV/DV，用 autoresearch 找模型 | 原 React 欄位映射與實際資料清洗、FIR 辨識、依前輪結果產生候選的有界自適應搜尋 | 不能從 tag 名稱或相關性自動證明操縱關係 |
| MPC 計算後回寫 MV | `mpc_campaign` 呼叫實際 `DmcController`，經 DA 寫命令並讀回，工廠推進後提供新 CV | 不自動切換 production engine 的 Control 模式 |
| 看 CV 持續優化 | 固定資料、評分與情境搜尋 tuning；明確選版後可人工安裝，實測績效另外觀察 | 不在線上邊控制邊自行改模型，不自動接管 |

## 架構與 module interface

### 1. 歷史資料 interface

標準交換檔為 JSON：

```json
{
  "sample_time": 10.0,
  "time_seconds": [0.0, 10.0, 20.0],
  "cv": [[50.0], [50.0], [50.2]],
  "mv": [[20.0], [21.0], [21.0]],
  "dv": [[], [], []]
}
```

每列是同一採樣時間；每個群組的欄位順序嚴格對應 `UnifiedModel.variables.cvs/mvs/dvs`。至少兩列、同列數、有限數值、正取樣時間、時間嚴格遞增且間隔一致。模型與歷史取樣時間必須一致。`provenance` 可附加來源、SHA-256、映射、授權與前處理資訊。

- CSV importer：明確指定編碼、時間格式及欄位映射，拒絕缺值、重複/跳時與非有限值。GBK 是輸入選項，不推測資料的控制語意。
- `validation/export_questdb.py`：讀取保存的 QuestDB `/exec` JSON，或使用者明確指定的 literal loopback HTTP endpoint。讀 `timestamp, node_id, value, status`，保留完整節點識別及數值精度；拒絕可能已碰撞的舊 `tag`／`field` schema，遷移時使用新 historian table。以明確 `max_age` 做因果零階保持；不讀未來值補過去，不跨過非 Good 品質，過期即失敗。
- 直接匯出保留原精度；避免 HMI 顯示用途的平均/四捨五入改變辨識資料。
- 現有 HMI historian 保留有效 OPC UA source timestamp 與精確來源 endpoint；來源時刻缺失或未設定時不以接收時間補成 Good 資料。舊版接收時間資料不能重新標記成來源時間證據。仍須評估時鐘對齊、採樣來源、deadband 與實際覆蓋率，不能據此宣稱已驗證真實控制器精準延遲。

初始條件假設為：第一筆之前的 MV/DV 已在基準值充分穩定，第一筆 CV 為對應基準。使用非穩態片段時須先準備足夠暖機歷史或另行設計狀態估計；不能靠每步強塞實測 CV 掩蓋自由運轉誤差。

### 2. 辨識與模型適用性

辨識使用多輸入有限脈衝響應及有界正則化搜尋，轉成現有 step-response JSON。`UnifiedModel` 增加可省略的 `control_strategy`；省略時仍為標準 DMC。CV/MV/DV 角色與 safety limits 由工程人員定義，不在搜尋空間內。

- 按時間分 train / validation / test。只用 train 擬合，validation 選候選，test 僅做選定模型的最後報告。
- 每個 CV 報告自由運轉 RMSE，對照固定初值/persistence 基準；另保存候選設定與失敗原因。
- 輸入不足激勵、常數或共線時拒絕辨識，不默認未知交互作用為零。
- 模型應保留 MV、DV 全部係數與單位一致性。必須明確覆蓋動態記憶長度。
- 現有支援範圍為有有限穩態增益的線性模型。積分型、未收斂 step tail、非線性工況切換不能靠最後一點截斷後聲稱有效；拒絕這類輸入。

**閉迴路歷史不是不可用，但簡單最小平方可能因回授、雜訊相關與不足激勵而有偏。** 留出擬合佳不代表已識別因果。需要已知控制架構、外部激勵/工具變數、適當閉迴路辨識方法或現場受控試驗補強；這些證據不由自動搜尋產生。

### 3. Twin implementation

令 `g[k]` 為在 elapsed `k * sample_time` 的單位步階係數；辨識輸出採 `g[0] = 0`，twin 亦可接收明確給定的非零直接響應。以 `h[0] = g[0]`、`h[k] = g[k] - g[k-1]` 對輸入相對初值做有限卷積；這保留長時間持續 MV 改變的最終增益，避免簡單丟掉舊 ΔMV 後 CV 錯誤回到基準。

先記錄當下 CV/MV/DV，再由控制器決定 MV；新命令經一個 sample interval 後反映到下一次 CV。不要把同一步的未知 CV 與 MV 作代數互相引用。離線與 OPC twin 必須採相同係數索引。

- `mpc_validate validate --plant ... --history ... --output ...`：已記錄 MV/DV 驅動 twin，初始狀態後不使用實測 CV 校正，輸出模型誤差與軌跡。
- `mpc_validate closed-loop --plant ... --controller ... --history ... --output ...`：DV 歷史重播；新 MV 來自實際 `DmcController`；CV 只由 twin 演算。
- plant 與 controller 為獨立輸入。相同模型只可作為數值一致性/軟體 smoke，不是工廠保真度證據。
- 若 DV 實際受 MV 明顯影響，不能一面固定回放一面聲稱閉迴路真實；需把該關係納入 plant。
- 不自動疊回歷史殘差。殘差可能含模型錯誤與閉迴路相關性，直接回放會造成看似精準；如另設擾動情境，必須獨立記錄及說明假設。

### 4. Autoresearch interface

[Karpathy autoresearch](https://github.com/karpathy/autoresearch) 原始 implementation 是 LLM 訓練實驗。本系統採固定評估器、依觀測結果迭代提出候選、保留證據與獨立測試的研究流程，不複製 GPU/LLM 訓練程式，也不允許研究代理任意修改控制程式或評分規則。

兩個搜尋迴圈分離：

1. **辨識**：搜尋模型記憶長度/正則化，指標是留出預測誤差。
2. **tuning**：`validation/research.py` 凍結獨立 plant 或經工程確認的歷史辨識假說、完整月份與評分尺度，調整 CV weight/alpha、MV weight_r。依前輪結果提出雙方向候選、縮小步長並略過重複點。至少三輪，且搜尋須覆蓋設定的調參座標；名義與不確定性情境在搜尋前固定。舊 `tune.py` 的有限候選 CLI 不冒稱自適應搜尋。

安全上下限、max_move、tag mapping、模式、CV 角色、評分權重、plant 模型與測試窗口不可由 tuning 搜尋變動。每輪保留候選 JSON、參數、固定指標與失敗訊息。非有限結果、solver 失敗或硬限不合格，不能以「零動作」當成功實驗。

研究分數使用固定工程尺度正規化的 **Var(CV − target)**、平均追蹤偏差平方、已定義品質規格的越限比例／帶外平方距離，以及命令增量平方；各情境等權聚合。另逐項檢查偏差、操作／規格違限、變異改善及 MV 累積動作代價門檻。**Var(CV)** 單獨報告；固定目標時兩者相同，變動目標時不能獎勵「CV 不動但不跟目標」。RMSE 不能冒稱變異改善，候選自己的 QP objective 不能作為跨候選評分。

輸出為 `candidate`，不是 deployment approval。多窗口與模型不確定性情境應固定後再比較；單一窗口最佳解不能宣稱全工況 robust。

### 5. OPC / ABB seam

`replay_plant` 是隔離 OPC UA client adapter：從模型指定 MV SP 取命令，將 twin CV、DV 與明確標記的模擬 MV feedback 寫入預先建立的節點。要求 literal loopback endpoint 與隔離確認；依 sample_time 真實時間運作、有限長度，不加速一端而讓另一端仍按牆鐘跑。

Live engine 以明確配置的 `node_ids.applied_sp` 作為模型 MV；未配置時只能使用 `node_ids.sp` 的命令暫存器回讀。`node_ids.op` 僅為 PID 輸出遙測，不能替代 SP。回讀同一 SP 不證明 PID／閥門已採用；需獨立有效 SP 訊號才能驗證該層採用與無擾動對準。接線前仍須核對操縱位置、工程單位及縮放。

OPC namespace、型別、每筆 status、讀寫權限、source/receipt time、模式握手與失連行為均為獨立整合驗收項目。不得自動把 `OperatingMode` 改為 2，不得觸碰現場端點。

`opc_da_simulator/` 提供原生 Windows COM in-process Server 與獨立 JSONL console bridge。bridge 使用 COM activation、`IOPCItemMgt`、`IOPCSyncIO`；不是把 JSONL 本身稱為 OPC DA。隔離模擬管理介面只負責設定物理輸入、品質、時間與故障；控制讀寫仍經 DA 介面。原生 Windows 可使用同一二進位；Linux 驗證在專用 Wine prefix 內執行，不讀寫使用者或現場的 COM 註冊。

本機 DA 驗收包括逐項 HRESULT、品質、時間、可寫權限、命令讀回、直接執行與 DA 執行的全軌跡一致性，以及 bad quality、斷線、拒寫後的停止/重新啟動。重新啟動不冒稱無縫重連。這個同步、本機、隔離 profile 不宣稱支援未實作的訂閱/DCOM/ABB 特有介面。

ABB 接線仍須確認實際型號、Server 版本、OP/SP 量綱、tag contract、模式映射及寫入授權。參考：[AC 800M OPC Server 6.0](https://library.e.abb.com/public/01118fad328ee7b3c1257dc00039f23e/3BSE035983-600_-_en_AC_800M_6.0_OPC_Server.pdf)、[ABB OPC UA 產品頁](https://new.abb.com/control-systems/system-800xa/800xa-dcs/fieldbus-protocols/opc-ua)。本地合成驗收不取代這些現場工作。

### 6. 三個月完整 campaign

CLI campaign 的範例設定為 `validation/examples/cement_campaign.json`；新研究工作區以該設定建立可編輯、不可變更版本的 study。程式由 UTC 起點及 calendar months 計算終點，不能以固定 90 天代替所有三個月。範例為 2026-01-01 至 2026-04-01、不含終點，60 秒取樣，共 129,600 個時間點。

- **真值工廠**：可設定的二階路徑、死時間、致動器延遲/速率/上下限、非線性、增益漂移、製程與量測雜訊。這些是假設的合成製程參數，不是從論文或 ABB 設備量測得到的常數。
- **激勵**：種子化 PRBS、緩變斜坡、工況切換與可量測 DV；保存完整情境與來源 hash。物理設定可以固定；CV 結果必須由每次模擬計算，不能預填曲線或改善百分比。
- **模型**：僅用 1 月資料辨識有限記憶 step-response；2 月選記憶/正則化與 tuning；3 月不參與候選選擇。控制模型不取得真值工廠方程參數。
- **輸入座標**：辨識以命令 SP 為輸入，因此包含致動器動態。DMC 使用最後成功讀回的命令 SP 作為 current MV；實際致動器值另存 `trace.mv`，命令另存 `trace.command_mv`，不能混用。
- **比較**：保留保守 baseline 候選，完整 2 月搜尋後凍結選擇；baseline、選定控制器與 DA 路徑均執行完整三個月。另獨立報告 3 月指標，不能用全期改善掩蓋留出退步。
- **故障**：完整無故障 campaign 之外，另外注入品質、斷線、拒寫情境並驗證拒絕後續動作與重新啟動。短故障案例不能替代三個月閉迴路。
- **可重現性**：保存設定、scenario、history、模型、候選、每次輸出、manifest、輸入/程式/執行檔 hash。`--resume` 必須核對來源及產物，不能僅因檔名存在而跳過。

### 7. 原系統 React 研究工作區

原 React 使用 **Autoresearch** 四步流程：匯入資料、清理與預覽、研究設計、結果與模型選用。獨立報告入口與結果頁的歷史報告連結均移除；既有磁碟產物及舊報告 API 保留供追溯。新研究 API 由後端強制 `ResearchManage` 權限，瀏覽器不能指定任意檔案或執行檔。

| API | 用途 |
|---|---|
| `GET /api/simulation-reports` | 列出已有 manifest 的報告 |
| `GET /api/simulation-reports/:id` | 期間、來源、切分、指標、候選、檢查與限制 |
| `GET /api/simulation-reports/:id/series` | `run/start/end/max_points` 篩選實際軌跡；UTC、終點不含 |
| `GET /api/simulation-reports/:id/artifacts/:name` | 下載 manifest 列明且留在報告目錄內的產物 |

UI 提供 CV/MV/DV、baseline/optimized/transport 疊圖、目標/上下限、命令/實際 MV、日期窗口及 zoom。圖表降採樣保留各通道區間極值；**指標使用完整解析度，不使用顯示抽樣重算**。手機側欄可收合；資料來源、三個月切分、測試指標、候選失敗原因與限制均須可見。

隔離報告執行使用 `AUTHRITY_HMI_CONFIG` 指向獨立設定；`runtime.connect_opcua=false`、historian 關閉、獨立 SQLite、loopback 服務地址。`web.bind_address` 與 `web.assets_directory` 可設定，既有預設不變。不修改現場設定、憑證或 production 端點。

### 8. 實際清洗與不可變資料

`validation/datasets.py` 將 CSV、canonical history 或 PostgreSQL 快照保存為不可覆寫原始資料，再根據明確設定產生新資料集。功能包含時間排序、去重、角色／工程單位映射、明確選擇 DA／UA／boolean 品質編碼、工程界線或訓練期 sigma 離群值判定、重採樣及有界填補。線性插值不跨資料切分；因果延續不使用未來樣本；長缺口保留 null 與 `usable:false`，不能刪除時間後冒稱連續資料。

清洗預覽提供原始／清洗後曲線、品質／填補計數與差異聚焦；圖表僅降採樣顯示，辨識讀完整 `history.json`。訓練統計只用 training 區間。研究工作再核對清洗切分、資料長度、取樣時間、訊號順序與單位，不能將用較長區間估計的清洗參數偷偷套入較短 training。

PostgreSQL 來源由管理員配置資料表／view、`timestamptz` 時間欄位及有序欄位白名單；瀏覽器只選來源與含起點、不含終點的 UTC 區間，不提供 SQL、DSN 或密碼。Rust importer 使用 repeatable-read、read-only 交易，保留原始品質、NULL、精確數字文字及微秒時間。筆數、cell 數、32 MiB 與 statement timeout 任一超限即整批失敗，不截斷後假裝成功。清洗資料保留來源、讀取時間、區間及內容雜湊。詳細設定與操作見 `validation/README.md`。

第 1 輪產品資料流程已用真實本機 PostgreSQL 與原 React 操作驗證：三個月 129,601 筆原始資料經 CSV／資料庫兩條路徑得到完全相同的 129,600 筆完整清洗資料；另確認唯讀交易阻止 view 內的寫入、失敗不留下半份資料、單位換算及桌面／手機恢復操作。圖表現在保留每個訊號的首個數值清洗差異及鄰近樣本，避免非極值修正被顯示取樣隱藏。證據位於 `validation/verified-runs/product-five-rounds/round1/verification.json`；這不代表後續建模、控制接管或工廠投運已驗收。

### 9. 方法選擇與控制成本

`control_strategy` 為 `{method,version,bands}`；方法只接受 `standard_dmc` 與 `soft_band`，版本為 1。Soft-band 的每 CV 設定為 `half_width`、`tracking_scale`、`quadratic_penalty`。品質帶以 live target 為中心；帶內保留縮小的追蹤權重，帶外有獨立非負 slack 二次成本。原操作 CV slack 與 MV／ΔMV 硬限制不變，不能以品質帶取代操作界線。標準方法的 bands 為空。

目前 Soft-band 僅接受 Target 模式；不支援的版本、經濟模式或 band 數量／值會明確失敗，不偷偷退回標準方法。相同 Rust 核心用於 twin、campaign 與 production；本次只驗證本機 campaign，不宣稱 ABB 投運合格。方法在 run/controller 建立時選定，沒有線上無擾動熱切換，也不把 MHE 或半小時固定延遲綁進此選項。

### 10. 研究任務生命週期

HMI `[research]` 預設關閉；啟用時要求此 HMI 的 OPC UA worker 與 historian 關閉，使用隔離的本機設定。管理員指定 Python、研究 script、Rust binary、COM launcher 與工作目錄；HTTP payload 不能覆蓋這些執行資源。Rust HTTP adapter 對接 Python 的實際資料／研究作業，不在請求或 MPC 掃描內跑長期最佳化。

Study 每次儲存產生新版本；run 凍結 study、資料來源與 runtime。一次僅一個活躍研究，瀏覽器關閉不終止研究。取消旗標令 worker 終止自己的引擎程序並記錄 cancelled；中斷或崩潰的部分試驗不當成成功，也不自動重播。最終候選只保存，不自動部署。

每種方法均完成整個選參月份，再凍結全部方法的勝出候選，之後才執行 baseline、optimized 與實際 COM DA 的完整季度評估。第三個月只用於最終比較。結果包含控制指令、實際 actuator、真實 CV、DV、目標、solver 狀態與 COM 呼叫證據；模擬品質不強制某種方法勝出。

驗收入口為 `python validation/verify_autoresearch.py`（使用者依操作文件在隔離環境手動執行）。檢查完整清洗、至少三輪自適應來源與座標覆蓋、雙方法完整季度、多情境門檻、逐掃描本機 COM 讀寫、硬限制、solver 與產物雜湊；另須實際瀏覽器桌面／手機操作。靜態型別檢查或單張 screenshot 不取代互動與完整閉迴路。


## 驗收順序

| Gate | 必要證據 | 不可推論 |
|---|---|---|
| A：資料准入 | 權利、欄位語意、時間、品質、激勵、獨立切分 | 下載成功不等於可辨識 |
| B：數學與軟體 | 延遲、長期增益、MIMO/DV 回歸，實際控制器與完整三個月閉迴路 | 軟體通過不等於水泥磨精度 |
| C：獨立歷史驗證 | 各 CV 留出誤差、baseline、操作區間、模型不確定性 | 同一模型自洽不等於工廠吻合 |
| D：OPC 整合 | 實際 UA/DA 通路、tag quality、命令後 CV 因果變化、失連行為 | 離線 solver 測試不等於 OPC 通訊通過 |
| E：現場唯讀 | 經核准讀取 ABB、預測/真值對照、不同工況 | shadow mode 不能證明未實施 MV 的反事實真值 |
| F：受控投運 | 明確現場寫入授權、PID 單位/模式、安全限、回退及逐步試運轉 | 模擬低分不授權實廠自動控制 |

錯誤的早先假設已取消：未知係數不能默認為 0；積分過程不能靠截短斜坡變成有限穩態模型；同一模型模擬不保證現場相同結果；`alpha=0` 只是參考軌跡直接指向目標，不代表實際 CV 一步到位。

## 資料與執行狀態

- 水泥磨資料調查：[validation/DATASETS.md](validation/DATASETS.md)。尚未有可宣稱通過准入的公開真實水泥磨資料，因此本次採明確標示的合成資料。
- 工具操作與指令：[validation/README.md](validation/README.md)。
- 專案預設由使用者手動執行 fish 指令。本次五輪驗收已取得使用者明確授權，由助理執行隔離的本機建置與測試。這不包含現場寫入、外部發布、正式部署或存取正式憑證；這些動作仍需個別的目標與動作授權。
- 本文件定義驗收契約；通過與否以實際 campaign 的 `report.json`、完整 traces、檢查紀錄及 UI 操作結果為準。尚未執行的項目不能填成通過。

## 2026-10-25 完整合成驗收結果

本次實際執行的通過報告為
[`validation/verified-runs/synthetic-cement-2026-q1/report.json`](validation/verified-runs/synthetic-cement-2026-q1/report.json)。
三個月歷史有 129,600 筆；baseline、optimized、native COM DA 各完成 129,600 次真實 DMC/Clarabel 計算，沒有以抽樣運算或預填結果取代。2 月四個 tuning 候選各跑完 40,320 scans。13 項 campaign 檢查全部通過。

| 3 月留出指標 | Baseline | 選定控制器 |
|---|---:|---:|
| 產品細度 RMSE（m²/kg） | 0.807687 | 0.495031 |
| 磨機差壓 RMSE（mbar） | 0.504965 | 0.195511 |
| 固定量綱正規化評分 | 0.000312714 | 0.0000693436 |
| 進料命令 total variation（t/h） | 1785.609 | 2490.894 |
| 分離器轉速命令 total variation（rpm） | 2277.025 | 7456.053 |

評分降低 77.83%，但命令動作量增加；不能將追蹤改善誤稱為所有成本都改善。
三條完整執行的 solver failure、MV 硬限違反及 CV 越限時間均為零。
直接與 DA 執行的時間、CV、MV、DV、目標及命令全軌跡最大差異均為零。
三種故障均在注入後被偵測並停止後續命令，另以新程序完成恢復驗證。

第一輪報告保留在 `validation/runs/synthetic-cement-2026-q1/`，狀態為 failed：
斷線故障原本誤帶單一 Tag 範圍，造成故障注入被拒，而非真正的 DA 斷線。
修正為 Server 層級故障後，在新目錄重新執行全部三個月流程；沒有覆寫原始失敗紀錄或手動改 pass。

實際驗證也包括：

- Rust MPC 55 項、Python 37 項及 HMI 7 項測試通過；原生 COM/JSON 測試通過。
- 修正真實控制器 live CV objective、未執行計畫誤入動態記憶，以及 twin 將模型記憶與預測視窗綁定的問題；後者保留失敗前/修正後的回歸證據。
- 原 React HMI 真實登入、變更密碼後重新登入、六個變數與三種執行疊圖、3 月窗口、單日 1,440 點及 zoom 720 點均已操作。
- 報告 API 的未登入請求回傳 401，無效路徑識別字回傳 400；下載檔 SHA-256 與 manifest 一致。
- 手機選單重選當前頁會關閉，圖例不覆蓋曲線；390px 視窗沒有頁面水平溢出。極小評分使用科學記號，避免四捨五入成零。

UI 操作證據、環境版本、原生 COM 證據與截圖在同一報告目錄的
`ui-verification.json`、`run-environment.json`、`native-interop-evidence.json`、
`report-overview.webp`、`report-charts.webp`、`report-mobile.webp`。
這些產物可由原 HMI 的報告頁下載。

本次本機報告服務為 `http://127.0.0.1:43173`，沿用原系統登入。
隔離登入資訊保存在本機權限 0600 的
`/tmp/authrity-campaign-tools.kti2k3/hmi/login.txt`；不要將它或 SQLite 帳號資料納入版本控制。
服務設定在同目錄 `settings.toml`，不連 OPC UA、不啟用 historian、不使用 production 憑證。
此 `/tmp` 執行環境是本次驗收環境；長期使用請依操作文件配置正式的本機報告儲存與服務啟動方式。

上述結果證明本合成場景的軟體閉迴路與本機同步 COM DA 路徑，不證明真實水泥磨模型精度、ABB 實機、網路 DCOM、無縫重連或工業安全認證。

## 自適應研究與雙方法完整驗收

新增功能的第一輪完整機器驗收已通過，與上方舊 campaign 分開保存：

- Root：`validation/verified-runs/autoresearch/attempt-1792935129101249264/`
- Run：`cf1e75f64fa14efd8dff543022fe5837`
- 證據：該 run 的 `acceptance.json`、`run.json`、`selection.json`、10 組實驗與命令紀錄、完整 baseline／optimized／transport traces。
- 原始資料由實際 Rust 製程產生，再加入明示的重複、亂序、Bad quality 與缺值作為清洗輸入。清洗後完整 129,600 筆資料實際供 FIR 辨識使用，並非只產生清洗紀錄。
- 標準 DMC 與 Soft-band 各完成兩輪依前輪最佳結果產生的候選，共 10 個全月試驗；兩方法的 baseline、optimized 與 COM DA 均各跑完 129,600 scans。8 項完整季度與傳輸一致性檢查通過。
- 所有完整執行皆無 MV／ΔMV 硬限制違反及 solver failure；直接與 DA 的時間、CV、MV、DV、target、command 軌跡一致。第三個月保留為未參與選參的測試期間。

| 3 月選定控制器，實際 COM DA | 標準 DMC | Soft-band |
|---|---:|---:|
| 細度 RMSE，m²/kg | 0.559430 | 0.983236 |
| 差壓 RMSE，mbar | 0.260105 | 0.543612 |
| Feed 命令累積動作量，t/h | 2129.074 | 1615.077 |
| Separator 命令累積動作量，rpm | 4589.539 | 4518.174 |
| 固定綜合分數 | 0.000103511 | 0.000393161 |

此設定下 Soft-band 減少動作但追蹤誤差較大，綜合分數未優於標準 DMC；未為了讓論文方法勝出而修改測試資料或評分。這是本合成場景的取捨，不是對真實工廠或所有調參範圍的結論。

本機原 React 研究服務：`http://127.0.0.1:43174`，設定為 `/tmp/authrity-campaign-tools.kti2k3/hmi/research-settings.toml`。其 OPC UA worker 與 historian 關閉；COM 僅連本機模擬 Server。`/tmp` 工具環境不是可攜部署產物，重建環境請依 `validation/README.md`。

### 原 React 操作驗收

- 1440×1000 桌面與 390×844 手機操作通過；手機頁面寬度與 scrollWidth 都為 390，沒有頁面水平溢出。
- 實際上傳三個月 CSV、指定品質欄位、拒絕未解決的缺值、啟用因果填值、比較清洗前後曲線，並將新建的不可變資料集帶入研究。
- 切換 Soft-band 欄位、儲存／重編輯研究、驗證錯誤界限提示，以及透過 UI 啟動與取消真實背景工作。取消 run `a73f193007c74f9cbe947d5aea3696e2` 持久化為 cancelled，沒有將部分結果標為通過。
- 實際切換留出期 RMSE／MV 動作量比較，以及 CV、DV、MV 指令與 MV 實際回饋曲線；指令和回饋使用各自的記錄，不共用假資料。完整期間及 3 月 1 日區間均有實際曲線。
- 歷史報告位於研究結果內，獨立側邊報告入口已移除；未登入研究 API 回傳 401，最終新開瀏覽器分頁無應用程式錯誤。
- UI 證據為上述 attempt root 的 `ui-verification.json` 與六張 `.webp` 截圖；UI 操作不由數值驗收程式假稱完成。
- 整合檢查：Rust MPC 63 項、Python 57 項（無略過）、HMI 14 項測試通過；React TypeScript／正式建置通過。最終以 `verify_autoresearch.py --check-run` 再核對凍結產物，結果為 passed。Vite 仍提示現有單一 JavaScript bundle 大於 500 kB；未以提高警告門檻隱藏此提示。

進入本機服務後選 **Autoresearch → 結果與報告**，再選狀態為 completed 的上述 run，即可查看完整研究與雙方法比較。此驗收只證明合成場景與本機 COM DA 路徑，未宣稱已符合 ABB 實機或真實水泥磨。

## 多變數獨立工程尺度圖表與完整預測封存

原 React 的 **控制器 → Trends** 與 **Autoresearch → 結果與報告** 已改用同一個 `ProcessTrendChart`，不是另建報告網站。

- 研究曲線預設只比較一種方法的 **基準 native ↔ 優化 native**，每個變數各列、比較組並排；手機改為上下排列。「方法比較」只顯示各方法的優化結果。「進階疊圖」才自行組合方法、基準／優化／COM DA 來源；傳輸路徑不是另一種演算法。
- 不同 CV／MV／DV 各用工程單位與獨立尺度。同一變數各比較組預設共用 Y 軸，允許明確解除並提示不可再以圖形高度直接比較；保留細節／含界限／手動／鎖定。預測殘差及逐步命令變化各用獨立零基準軸，不壓縮實體量測軸。
- 每條具體來源曲線都有勾選控制，可恢復預設、全選或清空；不可用來源停用並說明原因。製程比較與預測診斷各自保留選線設定，切換不會因另一個視圖的預設而隱藏所需資料。MV 的實際值、有效 SP 回授、命令與未來計畫不冒充同一量測；PID OP 不和 PV/SP 共軸。
- 品質規格、操作限制、Soft-band 與 DV 有效量程分開命名與選擇；上下限線和帶狀填色可獨立開關。只有已配置且品質良好的成對上下界才填色，不從操作界限推定品質規格，也不跨缺值補畫。
- 完整期間總覽以時間桶的 Good 觀測中位數及最小～最大值呈現，陰影不是信賴區間。訓練／驗證／測試界線來自該研究實際分割；拖曳後明確套用選取，亦可用鍵盤指定區間、以事件／游標聚焦 6h 或 24h、重設全期。所有詳細圖與副圖共用時間窗及游標。
- 選定視窗統計由完整解析度資料計算，絕不用顯示減點結果估算：CV 實體母體變異／標準差、同時刻目標偏差、各自的品質／操作違限及涵蓋樣本時長，MV 則呈現累積絕對命令變化與有效相鄰配對數。樣本時間窗包含兩端；違限及涵蓋秒數為可比較 Good 樣本數乘掃描週期，不是插值後的連續持續時間。
- 無品質規格、無有效違限比較或沒有有效相鄰命令配對時，對應統計為 `null`，不是零；有實際涵蓋且沒有違限／命令不變才可顯示零。MV 差分只使用同視窗、相隔一個掃描週期且品質良好的相鄰命令，不跨品質或資料缺口。縮放只改變觀察範圍，不改封存評分、留出驗證、候選選用或控制參數；動作量不是能源。
- 新執行由 Rust 每個 scan 串流封存完整 CV horizon、前次 MV base 及 solver MV increments，另存定位索引與雜湊。CV 第一點在起算後一個 sample；MV 第一個計畫動作在起算時刻。快照顯示當時真正算出的預測及後續已記錄的實際反應，不用現在的模型重算歷史。
- 歷史模式可選固定提前步數；完整快照可由游標載入。殘差只配對同方法／版本、相同有效時間且品質良好的量測，不以插值偽造配對。
- Historian 顯示抽樣保留極值、品質／設定變更與真實缺口，不用平均值抹平尖峰。清洗和控制績效仍使用完整資料。舊 run 沒有完整封存就明示不可用；既有 live `PredictionGraph` 因缺少發佈時間契約而維持原功能。
- OPC UA historian 中舊 `Prediction`／`FuturePlan` array 缺少 solver 起算時間與 sample interval，不能冒充有效時間已對齊的歷史預測。工程單位欄位只改顯示標籤，不做數值轉換。

### 本次完整驗收

- Root：`validation/verified-runs/independent-trends/attempt-1792980609772715393/`。
- Run：`d56d9ec846ca43208cb3adc6793e4442`；`acceptance.json` 為 passed。
- 標準 DMC、Soft-band 各完成 baseline、optimized、COM DA，六組各 129,600 scans；兩輪自適應搜尋共十次試驗。八項完整季度／傳輸檢查通過，直接與 COM DA 的六類軌跡最大差異均為 0。
- 完整驗收同時核對六組預測封存與索引的雜湊、scan 數、首／中／末紀錄及時間對齊。本場景 CV horizon 為 48 steps、MV control horizon 為 4 steps、sample time 為 60 秒；這些數字來自凍結設定，不是圖表常數。
- 另將已完成 COM DA 的歷史軌跡載入真正的隔離 QuestDB，共 3,369,560 筆欄位紀錄。測試副本明示注入一筆 Bad quality 並移除一個 CV 的 40 筆紀錄；不改原始證據。季度單一 CV 的 129,560 筆降至 14 個顯示點時，來源 min/max、Bad quality 及缺口均保留。
- 原 React 實際驗證 1440×1000 桌面與 390×844 手機：六條獨立軸、三軸疊圖、手機分軌、手動錯誤界限、鎖軸、線種切換、共用游標、重複縮放／重設、單位編輯、缺口與品質標記、原始資料過量拒絕及縮窗恢復、舊封存不可用提示均已操作。
- 新封存快照實際顯示 48 點 CV 預測及 4 點 MV 計畫；首個 MV 計畫值等於該次真正下達的命令。五步提前曲線的有效時間逐點比起算時間晚 300 秒。游標同時顯示發佈／有效時間及同源殘差。
- Rust MPC 68 項、HMI 19 項、Python 68 項（無略過）、前端 7 項測試通過；React TypeScript 與正式建置通過。Vite 既有大於 500 kB bundle 警告仍保留。
- 第一次嘗試 `attempt-1792977761514252342` 在 COM DA 收尾收到 SIGKILL，保留為失敗，沒有改寫成通過。當時記憶體／swap 接近耗盡；把本次工具快取移出 RAM disk 後，以全新目錄完成上述驗收，未降低 scan 數或檢查標準。

圖表操作證據與截圖保存在 `validation/verified-runs/independent-trends/ui-verification.json` 及同目錄的 `.webp`。

研究檢視入口為 `http://127.0.0.1:43175`，選 **Autoresearch → 4 結果與報告 → Complete three-month acceptance**。Historian 檢視入口為 `http://localhost:43176`，選 **ChartVerification → Trends**，查詢 UTC `2026-01-01 00:00:00` 至 `2026-04-01 00:00:00`；長區間使用保留極值的顯示抽樣。兩個隔離測試帳號均依測試要求設為 `admin`／`admin`。

研究與 Historian 使用分開的本機 HMI 實例，保留「research 不與啟用的 historian／OPC worker 共置」安全規則；未為展示而移除保護。相關本機設定為 `/tmp/authrity-campaign-tools.kti2k3/hmi/chart-research-settings.toml` 與 `/tmp/authrity-campaign-tools.kti2k3/chart-hmi/settings.toml`，不是 production 部署設定。既有 `43173`／`43174` 歷史檢視服務亦使用新後端，避免新前端對到舊 series API。

本次證據仍只涵蓋合成製程、本機 COM DA 及 historian／React 資料路徑，不證明 ABB 實機、遠端 DCOM 或真實水泥磨的控制精度。

## 第 3–5 輪產品化契約與本機驗收

### 選版、安裝與啟動分離

研究完成不代表投運授權。`validation/control_versions.py` 重新核對候選門檻、留出測試與來源產物，再將精確模型 bytes 與 manifest 保存成不可變控制版本。來源／模型改動、失敗或未完成研究、留出測試參與選參，均不能冒稱合格版本。舊執行的結尾來源 hash 不冒稱啟動時快照。

原 React 結果頁先顯示變異／偏差、採用評估與模型選用；完整試驗和額外指標收合。安裝另需明確確認既定 supervisor；HMI 驗證回覆中的模型、manifest 與版本雜湊，不因 HTTP 成功就認定正確安裝。安裝不啟動 engine、不建 OPC 節點、不要求 Engage。啟動時 supervisor 再核對模型 bytes，engine 對同一讀入 buffer 驗證 SHA-256 後解析。

手動「儲存設定」只建立另一個未經研究准入的模型。Supervisor 固定傳入手動模型輸出目錄，engine 不從不可變檔案的 parent 推測位置；獨立 CLI 需明確指定 `--model-output-dir`。設定一次批次讀取並檢查品質、完整性與範圍，保留原 controller 名稱及方法／節點映射，不覆寫既有檔案。它不是新的合格研究版本。

Generator 的 JSON 匯入與 YAML 匯出保留控制器身分、維度順序、取樣時間、精確 node IDs 及求解設定。YAML 內的 `model_binding` 是 HMI 變數分類及語意權限來源；缺值／歧義直接拒絕，不以節點後綴猜測。部署節點會更新清單，但不啟動控制器。實際 admin 測試中，PV、實際 PID mode、prediction、AcquisitionSequence 及未配置節點均回覆 403；CV target 寫入才回覆 OPC 接受。

Historian 以原樣 `node_id` 儲存，未四捨五入；`PV`、`unknown:PV`、含逗號的 ID、底線及 `System:` 前綴已有實際 OPC → QuestDB → HMI 讀回證據。內部 metadata 使用明確標記排除，不根據物理節點名稱刪除。

### 控制保護與資料生產者

- 接管前須有健康 Monitor 求解，之後由操作員明確要求 Engage；連線恢復或故障清除不自動恢復控制。
- 每個控制器配置一個外部 `AcquisitionSequence` Double 節點。唯一資料生產者在採集前發布奇數，完成整組 PV／DV／有效 SP 後才發布下一個偶數。靜止初值、engine heartbeat、值未變或單純接收時間不能代替採集新鮮度。
- 讀取用同一穩定偶數包住整組資料；發布中的奇數／跨代讀取只在限時內重讀。命令前再讀模式、有效 SP、tuning、界線及量測；量測可前進，但命令契約不能悄悄改變。
- 未完成的整組命令不得被下一組覆蓋；模型 scan 間仍輪詢應用確認，確認截止時間不隨重試延後。壞品質、停更、模式失效、越界、求解失敗或未確認寫入均停止新命令並要求重新觀察／明確接管。
- `OperatingMode`、`Heartbeat`、`NextRun`、`SolverStatus` 依既有 schema 寫入 `Int32`；求解碼 0 為 Solved、1 為 AlmostSolved。狀態及預測均不授予控制權。
- `virtual_plant` 與 `replay_plant` 的理想追隨／採樣模型不證明真實 PID 或閥門動態。既有示範 JSON 的工程限制仍須獨立審查；例如 VP_CSTR 的零 `max_move` 與不一致的硬界線不能靠放寬驗證器自動通過。
- Native engine 使用明確指定的 `--pki`，不自動信任伺服器。未釘選憑證時已實際拒絕連線且沒有初始化／MV 寫入；核對隔離伺服器公鑰憑證後，才從原 UI 成功啟動觀察與接管。

### 操作確認與實測績效

`POST /api/runtime/write` 取代 WebSocket WRITE。登入、CSRF、節點寫入權限、排隊／執行期限、數值原生型別與每項 OPC StatusCode 均有明確處理。只有伺服器回覆一筆 Good 寫入才回傳接受；一般 Tag 更新不清除 pending，逾時不呼叫成功回呼，也不自動重送。這仍不是 PID 已採用 SP 的證明。

`{engine}:ControlObservation` String 節點承載實際有效掃描的滑動視窗：CV 實體變異、目標誤差變異、偏差、RMSE、操作違限筆數及有效 SP 累積動作量。`--monitor-window-samples` 控制視窗；模式、目標、界線或觀察歷史失效會重置。統計不是命令權限，也不是節能量或相對基準改善率。時間為 engine 收件時間，不冒充 PLC source timestamp。

原 React 遇到斷線、非 Good 品質、無效／過期觀測會隱藏數值；視窗尚未完整就明示累積中。管理頁只呈現請求模式，不把 `OperatingMode=2` 直接標成已接管。所選模型顯示真實 P／M／取樣時間及全精度 tuning 對照；P／M 不再提供沒有引擎支援的線上編輯控制。

### 驗收狀態

第 1 輪已完成 CSV、真實唯讀 PostgreSQL 匯入及原 React 清理驗證，三個月 129,601 筆原始紀錄形成 129,600 筆清理資料，完整比對 CV／MV／DV 與來源追溯。第 2 輪獨立工廠組 `36b87b02d28e40e2a3075bcfab5be79d` 已有完整驗收產物；歷史辨識組 `6987fdbcebad44cbbf2e1a8e87d78219` 已完成 95/95、16 項執行檢查及獨立 verifier 核對。兩組都是合成資料，不是實廠保真度證據；歷史組的執行中來源封存也不冒充啟動時快照。

目前來源的季度重驗 run `888c83de64c64f1ab04d9210c58762e1` 已完成 94/94：兩種方法各 129,600 筆、6 輪自適應搜尋／26 個試驗、三種增益情境及 16 項執行檢查均通過；直接 native 與實際 COM DA 的 time／CV／MV／DV／target／commandMV 最大差為 0。證據位於 `validation/verified-runs/product-five-rounds/current-source-acceptance/attempt-1793073242021806812`。這不是沿用舊 binary 的結果。

原 React 已明確選用並安裝 `standard_dmc-trial-011`，不可變模型 SHA-256 為 `82e494a66787284c7325d3fea9b23e1746f6884d95d4c6ae0ff925c28c0f8a99`。隔離 native engine 已通過健康 Monitor，操作員要求 Engage 後產生並確認多組 SP；獨立線性化 SOPDT／致動器延遲模擬器的 CV 隨 MV 與 DV 動態演進。這項即時路徑使用明確較弱的 SP 暫存器回讀，不是 ABB PID 採用證據，也不取代季度非線性驗收。

第 3–5 輪的本機控制與原 React 桌面／手機驗收已完成，證據集中於 `validation/verified-runs/product-five-rounds/round3`、`round4`、`round5`：

- 另以 **1 CV／1 MV／0 DV** 的未送審協定測試模型，實際驗證獨立 `applied_sp`：確認逾時會 Hold；回授稍後追上也不自行解鎖；回到健康觀察並明確接管後，連續命令均得到獨立 SP 回授確認。這是接受設定值的證據，不是實體閥門已動作的證據。
- 停止資料生產者、凍結調參不符、真實 OPC `BadNoCommunication`、缺失採集節點及撤回 PID Remote Cascade 均已走實際引擎路徑。保護期間核對 SP 值與寫入時間戳未再改變；恢復品質不自動接管。中斷後舊 producer 的 session 確實失效，採明確重新初始化，不冒稱製程狀態無縫延續。
- 有限的獨立 SOPDT／致動器延遲模擬器已完成並退出 0；兩個 native 測試控制器均已明確停止。實測視窗曾完成 6/6 筆，原 UI 顯示 CV 變異、偏差、違限、有效 SP 動作量及使用實際取樣時間的預測；失效後顯示觀測不可用。
- 實際發現並修正 supervisor 將主動停止誤判為 `Failed / Exited with code: -1`。現在由擁有 child handle 的 monitor 接收停止請求，確認退出才釋放控制器位置；不再依外部 PID 發送命令。實際 native 程序與 React 回到 `Stopped`、`last_error=null`；真正的非零退出及意外訊號仍判失敗。永久 real-child 回歸及全部 11 項 supervisor 測試通過。
- 結果頁先呈現留出變異與 MV 代價，再看曲線、試驗、選版。六個 CV／MV／DV 圖各用獨立工程尺度；手機改一個軸不會改其他軸。390 px 實際驗證操作開關可見、PV／Target／SP 優先、表格獨立捲動且整頁不橫向溢出。
- 圖表不再為沒有資料的品質類別建立空曲線；同一完整季度手機檢視的單次繪圖觀測由約 10.1 秒降至 1.58 秒，沒有刪除模擬樣本或縮短測試期間。實際壞品質點仍有標記。這是該次瀏覽器量測，不是所有硬體的效能保證。

### 結果解讀與操作入口

本次驗證分數優先的候選是 `standard_dmc-trial-011`，不是因為新方法名稱就優先推薦 soft-band。以下為 **standard_dmc 自身基準與調整後 COM DA 的獨立留出模擬**：

| 工程指標 | 基準 | 調整後 | 變化 |
| --- | ---: | ---: | ---: |
| ProductFineness 實體變異 `(m2/kg)²` | 10.4092 | 10.3267 | 降低 0.792819% |
| MillDifferentialPressure 實體變異 `(mbar)²` | 0.928083 | 0.876067 | 降低 5.60464% |
| FreshFeed 累積絕對命令變化 `t/h` | 1,785.61 | 1,831.75 | 增加 2.58366% |
| SeparatorSpeed 累積絕對命令變化 `rpm` | 2,277.05 | 2,479.03 | 增加 8.87047% |

追蹤誤差變異不是上表的實體變異，MV 動作量也不是耗能；不同方法的改善百分比使用各自基準，不能單靠百分比排名。這次降變異有動作代價，不宣稱全面優於基準。

目前使用原 React 的兩個隔離工作區，取代前節舊版驗收入口：

- 研究：`http://127.0.0.1:43179` → **Autoresearch → 4 結果與報告 → Complete three-month acceptance**。亦可在步驟 1 讀取已配置的唯讀 PostgreSQL 三個月合成資料或匯入 CSV。
- 控制：`http://127.0.0.2:43178` → **synthetic-cement-2026-q1 → Manage／Overview／Trends**。目前程序已停止；historian 與封存結果可檢視，沒有留下無人接管。
- 隔離測試帳號均為 `admin`／`admin`。保留研究與即時 OPC／historian 不共置的安全規則；沒有為合併入口移除保護。DSN 與 supervisor secret 不放入前端或驗收產物。

完整原始結果、不可變版本、故障前後時間戳、實際 UI 截圖及目前 executable 身分見 `current-source-checkpoint.json`。驗收涵蓋可配置維度的軟體流程、合成水泥磨、真實本機 COM DA 與 OPC UA，不證明 ABB 實機、遠端 DCOM、實際閥門動作或各水泥製程的控制精度。現場仍須各自建立模型、量測品質／採樣契約、PID 權限與安全停機程序；停止 MPC 程序不會替 DCS 自動切換 PID 模式。


### 曲線分面與選線驗收

此更新使用已封存的 `888c83de64c64f1ab04d9210c58762e1`，沒有重新執行或宣稱改善 MPC 最佳化。變更限於唯讀曲線轉換、視窗統計及原 React 呈現；不可變模型的 SHA-256 與原驗收一致，原研究來源身分及評分維持封存紀錄，不改寫成新來源執行的結果。

- 完整季度 API 的 129,600 筆 CV 實體變異／偏差、129,599 個有效 MV 命令配對，以及六個變數總覽的原始極值／桶內中位數，均與完整原始軌跡獨立核對。縮至 6 小時的含端點視窗亦核對 361 筆資料，總覽與原評分不變。
- 實際操作原 React 的基準／優化分面、僅優化方法比較、六組方法／來源進階選擇、逐條選線／清空／恢復、跨區間及重整保存、獨立診斷設定、手動解鎖與重新共軸。三變數疊圖仍保留三個不同工程軸，不做共同尺度正規化。
- 桌面 1440 px 與手機 390 px 均完成實際操作。手機比較組上下排列，改 ProductFineness 的範圍不改 MillDifferentialPressure；選線展開後亦無整頁水平溢出。
- 總覽以鍵盤移動選框時不送請求，按套用才載入所選兩組資料。事件依類型與比較組篩選，點選後所有圖同步聚焦。實測發現的快捷縮放後輸入欄殘留舊區間、游標留在窗外問題已修正，並由原 UI 重驗。
- 五步提前預測的有效時間與發佈時間相差 300 秒；單次封存快照保留每 CV 48 點預測、每 MV 4 點計畫，殘差與後續同時刻實際值一致。這些維度來自該封存模型，不是圖表常數。
- 該季度沒有配置品質規格，UI 正確停用品質規格帶；實際核對操作限制及 Soft-band 各自的填色。既有隔離 historian 的五筆 `BadNoCommunication` 紀錄仍顯示為五個紅點，不連成 Good 曲線；沒有完整解析度統計的來源不以減點結果冒充績效。
- Python 曲線回歸 **21/21**、前端回歸 **24/24**、TypeScript 與正式建置通過。既有大於 500 kB 的 bundle 警告保留。

驗證紀錄位於 `validation/verified-runs/chart-presentation/`：`backend-checks.json`、`ui-checks.json`、`regression-checks.json`，以及實際桌面、手機、預測診斷與 historian 截圖。操作入口仍為原研究工作區 `http://127.0.0.1:43179`，重新整理後選 **Autoresearch → 4 結果與報告 → 查看曲線**。

## 數位孿生工作台：辨識 → 閉環調參 → 選定候選重驗

本節定義本次實作與驗收契約；完成狀態以本節末的實測證據為準，不沿用前節的舊版建置結果。

### 操作順序與責任

1. **資料準備**：沿用原 React 的 CSV／唯讀 PostgreSQL 匯入、欄位角色映射、品質／缺值／異常處理與不可變清理資料。CV／MV／DV 與工程限制由使用者確認；不從相關性自動宣告操縱關係。
2. **辨識與准入**：按時間分割資料，訓練區辨識 FIR／步階響應。歷史資料模式再擬合各 MV→CV／DV→CV 的 SOPDT 路徑 `gain / tau1 / tau2 / delay_seconds`，建立獨立的製程模擬假說；控制器使用自己的步階係數矩陣。顯示辨識與自由運轉驗證結果，不將製程模型參數與 MPC 權重混為一談。
3. **Autoresearch**：固定製程、初值、情境、限制及評分規則。每個 trial 的 CV 權重倍率、MV 權重倍率與 alpha 在該次情境內固定；實際 MPC → MV → 製程狀態推進 → CV 回授反覆執行後才評分，再提出下一個 trial。不是先猜完參數，最後才跑一次模擬器。
4. **候選凍結與留出驗證**：所有方法完成選參後才讀取最終測試分數。選定控制器 bytes 不可變；完整情境、不同不確定性情境與 COM DA 數值等價性使用既有驗證程序。
5. **數位孿生**：在同一 React 工作流程中選取研究、方法與 trial，查看封存軌跡，或以該 trial 的精確控制器 bytes 建立新的閉環執行。新執行不覆寫 trial、不重新套用權重倍率、不改變原研究的准入或部署狀態。

模擬器同時是搜尋期間的評估器及選參後的重驗工具。歷史資料回放仍不能代替受新 MV 驅動的製程動態。若看過留出結果後繼續調整，後續執行明確屬於探索，不再宣稱同一測試集未被使用。

### 原 React 的工作台

- 保留既有匯入／清理／研究設計／結果功能，新增「數位孿生」工作步驟；不新增獨立網站或 HTML 報告入口。
- 模型區顯示實際封存的 SOPDT 路徑矩陣、辨識結果與來源範圍。合成資料標示合成；歷史辨識標示觀測假說，不顯示不存在的工廠保真度認證。
- 控制器區用 trial ID、方法、輪次、參數、是否准入與拒絕原因選擇版本。可查看非優勝的有效 trial；不合格 trial 的診斷重跑不等於可以部署。
- 現有製程區只有「MV／DV → 動態製程 → CV」模型拓撲，並非設備畫面；不能算作使用者要求的水泥磨設備 UI 已完成。設備層改採下方「3D 水泥磨數位孿生看板」設計，取代先前不足的拓撲交付。未建模的設備電流、溫度、振動等仍不得生成假值。
- 狀態只由已收到的引擎快照更新。暫停／斷線／失敗停止動態指示；零筆資料顯示尚未執行，不顯示虛構正常值。
- 封存檢視與新閉環執行明確分開。歷史 trial 原本只有 validation 情境時，不擴充標籤成完整三個月；需要完整軌跡時建立新的 full 執行。
- 播放以「每次更新掃描數」設定加速，不冒稱牆鐘即時。每步仍使用原 sample_time；不能靠跳過 solver 或製程掃描加速。
- 播放為序列式推進請求；暫停或離開畫面停止發送，已開始的有限批次可完成。重新取得快照後才續跑；失去回覆不自動重送寫入。顯示進度與確切已執行筆數。
- 執行中的曲線是最新最多 256 筆實際掃描，明示滾動視窗；完成後使用完整封存軌跡與完整解析度統計，不以顯示減點計算績效。
- 比較封存 trial 或完成 session 時保留各自來源；session 必須有相同 plant／scenario／時間原點／採樣週期及變數契約。不能把不同擾動或初值造成的差異歸因於調參。

### CV／MV／DV 三區圖

在既有 `ProcessTrendChart` 增加 `role_blocks`，不建立第二套圖表數據契約。

- CV 在上、MV 在中、DV 在下；共用時間範圍、游標與物理 X 繪圖範圍。
- 每個角色至多兩個變數可使用左右獨立工程 Y 軸；更多變數及手機使用角色內子泳道。不同變數不共尺度、不暗中正規化。
- 同變數跨候選預設使用相同 Y 範圍；比較組以完整三區並排，手機上下排列。解除共軸後明確提示不可只比較曲線高度。
- 保留逐條選線、手動／鎖定軸、限制帶、時間縮放及品質斷點。未配置 DV 與使用者隱藏 DV 是不同狀態；不補零線。
- CV 預測按有效時間 `issued_at + lead × sample_time` 顯示；當下預測不是當下實際 CV。未封存的未來曲線不可重算後冒充原始預測。

### 執行 module 與 interface

**Rust 控制／製程 module**：將 `campaign::closed_loop` 的同一掃描流程封裝為有狀態執行器。批次 CLI 與新的 `mpc_campaign session` 共用它，保留延遲、致動器、DMC 歷史、命令讀回、求解／界線檢查及預測封存；不在 React 或 Python 複製物理模型。

**Session interface**：stdin/stdout JSONL；`snapshot` 不前進，`advance` 要求 `expected_samples` 與目前筆數相同並限制每次 1–4096 掃描，`close` 釋放資源。回覆帶 request ID、狀態、已執行／要求筆數、最新 frame 與有限近期軌跡。序號不合拒絕且不前進。正常完成才有 completed／valid；關閉未完成執行保留中斷證據，不能留下舊成功結果。

**HMI session module**：只允許既有隔離研究配置；沿用登入、研究權限、CSRF 與 audit。伺服器獨占 child 與命令序列，限制活動數量及命令時間；不接受瀏覽器提供路徑、executable、bridge 或 OPC endpoint。重啟後不從一列 CV 猜測內部狀態，活動 session 標示中斷；已完成封存仍可讀。

**封存 module**：每個新 session 有獨立目錄，凍結來源 run／trial、實際 controller／plant／scenario bytes 與 SHA-256、執行 binary 身分、時間與傳輸模式。重跑使用精確控制器，不再次乘 tuning 倍率。新程式重跑舊 trial 必須顯示新 binary 身分，不將新結果改寫成原研究證據。

HTTP interface：

| 路徑（`/api/research` 下） | 方法 | 行為 |
| --- | --- | --- |
| `/runs/:id/twin` | GET | 模型、辨識、trial 與 session 清單 |
| `/runs/:id/trials/:trialId/series` | GET | 該 trial 實際保存的情境曲線 |
| `/twin/sessions` | POST | 選擇來源 trial、selection／full 情境及 direct／opc-da，建立暫停的執行 |
| `/twin/sessions/:id` | GET | 最新快照；不計算下一步 |
| `/twin/sessions/:id/advance` | POST | 以已知筆數前提推進有限掃描 |
| `/twin/sessions/:id/close` | POST | 明確結束，未完成不當作成功 |
| `/twin/sessions/:id/series` | GET | 完成後的完整封存曲線 |

### 本次驗收條件

1. 完整 129,600 掃描季度，原批次與分批 session 對相同輸入產生相同 time／CV／MV／DV／target／command／prediction／quality 軌跡；求解牆鐘時間不要求相同。不縮短成短程 smoke。
2. snapshot、暫停與序號不符不增加掃描；實際失敗及提前關閉不標成功；完成檔可重新開啟。抽查原始 trial/controller bytes 保持不變。
3. 經原 HMI 的真實登入／CSRF 介面建立與推進 session，驗證精確模型、完整軌跡與來源身分；無權限、錯誤 ID、跨情境比較不能悄悄通過。
4. 實際原 React 桌面與手機操作 trial 選擇、封存比較、建立／單步／播放／暫停／關閉／完成重開；圖上的 CV 由實際 Rust 計算更新，不能只動畫回放。
5. 三角色順序、獨立 Y 軸、共同時間像素位置、逐條選線、手機無整頁橫向溢出與瀏覽器錯誤均實際檢查。驗收產物保存至 `validation/verified-runs/digital-twin-workbench/`。

本次不授權接管真實 ABB、遠端 DCOM 或生產部署。既有現場觀察／安裝／Monitor／Engage 保持獨立；工作台不能繞過它們。

### 本次實作與驗證交接狀態

- 已加入共用 `CampaignRunner`、JSONL session、HMI session routes、不可變 trial/session 封存，以及原 React 第 5 步數位孿生。新版 HMI 已實際完成從 CSV 到完整季度研究、選版與同模型新 session 的端到端操作；證據見下方影片與結果索引。
- **設備 UI 尚未完成**：上述端到端證據只涵蓋研究、閉環執行與趨勢工作台；目前沒有 2D 設備圖、3D 設備場景或設備動畫。下方新增的是設計契約，不是實作／驗收證據。
- 已觀察 native `mpc_campaign` release 建置成功。新 binary SHA-256：`d36b7a7040833cb067239ceaa5ee0da0b09a3c59b6ae1fc76fc1d29b03ad6fdc`。
- 使用者執行更新後的前端 TypeScript／正式建置及 28 項回歸全部通過，包含雙軸圖例變數名稱的變更；最新 assets 為 `index-D1aSjiWU.js`／`index-BnmpP7n0.css`。既有大於 500 kB 的 bundle 警告未隱藏。
- 使用者重跑 Python 回歸：148 項、`OK (skipped=4)`，原先三個缺少 `limits` 的錯誤已排除。測試使用正式 `controller_template` 產生完整模型，新增凍結控制器限制優先於研究設定、限制線及越限秒數的回歸；沒有在正式圖表程式填入預設限制。
- 鎖檔修正後，使用者以 `--locked` 完成 HMI release 測試與建置：研究相關 20 項通過、25 項未納入該次篩選；新版 `127.0.0.1:43180` 已啟動，瀏覽器實際登入原 React 成功。
- 原 React 已建立標準 DMC 的完整季度 Direct session `e885531940c44deda596753e45e4d84c` 與 COM DA session `c3ade3d2ce974ff9817b2852a3afc444`。兩者均完成 129,600 掃描，並與同 run 的原生批次 `standard_dmc-optimized.json` 比對：time、CV／MV／DV、target、command、prediction、quality、solver status 逐筆完全相同，完整 `forecast.ndjson` SHA-256 亦相同；求解牆鐘耗時不納入數值等價。COM 證據含 129,600 次 write／update、259,200 次 read，讀寫錯誤皆為零。詳見 `full-quarter-direct-com-equivalence.json`。
- 實際工作台桌面雙組三區圖的同組 X 邊界分別為 x=379..756、x=928..1305；390 px 手機各變數子泳道均為 x=94..284，scrollWidth=390。已操作預測選線與隱藏 DV，設定在後續實際掃描更新中保持；各變數仍使用獨立工程 Y 軸。
- 已驗證快照不推進、單步恰加一筆、暫停後筆數不變。過期序號帶回錯誤但不推進，超出 4096 的批次遭拒；接續有效單步仍成功。另以專用工作階段執行 2 筆後明確關閉，原生結果為 invalid，原因 `Session closed before completion`，UI 不允許當成完整封存；此案例不是季度績效證據。
- 來源研究 `572258edf60544bfafd68c69bee1a1bc` 已完成 26 個候選的六輪搜尋，標準 DMC 選定 trial-011、Soft-band 選定 trial-024。其獨立 `soft_band-transport` 批次雖完成 129,600 筆並回傳最終 COM 統計，關閉 bridge 時卻收到 SIGKILL，研究因此在 91/95 判為 failed；不改寫失敗結果、不宣告研究完成。主機同時有 RAM／swap 壓力及 kernel allocation failure，但尚未取得 SIGKILL 發送者的確證，不能把記憶體壓力當成已證明的唯一原因。
 較早由原 React 建立的 Soft-band Direct session `6eb1037cb07e4569a10af1ffb12c2c11` 與 COM session `62ed0ffc56d74825ac185f3cd94d6e12` 均已正常完成 129,600 筆且成功關閉；與 `soft_band-optimized.json` 的九個 trace 欄位及完整 forecast SHA-256 完全一致，見 `full-quarter-soft-band-equivalence.json`。這份成功證據不覆蓋前述另一個失敗批次。
- 整合時修正 `UnifiedModel.variables` 的實際 `cvs/mvs/dvs` 與 `units` schema，session 准入檢查名稱、順序、單位與維度；沒有使用 UI 的 `cv/mv/dv` 欄位假造控制模型。模型尚未產生的正常訓練階段顯示空路徑／未有准入證據，不虛構預設 plant。
- **仍待取得的輸出**：native `campaign_tests` 與 4 項真實引擎 Python 永久回歸。4 項原先略過的原因是未設定 `MPC_VALIDATE_BIN`；不將略過當作通過，也不以完整季度運行冒充永久測試輸出。先前 `/tmp` 補驗腳本在續接環境已不存在，`VIDEO_GUIDE.md` 提供不依賴該腳本的 fish 命令。

最新完整證據位於 `validation/verified-runs/digital-twin-workbench/` 的 `filmed-research-completion.json`、`filmed-session-equivalence.json`、`ui-video-manifest.json`、`video-delivery-integrity.json` 與 `VIDEO_GUIDE.md`。`build-checkpoint.json` 已同步完成狀態及未取得的測試輸出；早期 `ui-checkpoint.json`、`application-progress-checkpoint.json`、`current-source-research-start.json` 等仍是歷史 partial checkpoint，不能取代最新結果。

### 閉環工作台 UI 操作影片驗收（不含設備場景）

使用者要求由開始到完成的真實操作影片及說明。同一 CSV／研究／trial／session 的 18 段原 React 實錄已保存並解碼核對，順序列於 `ui-workflow.m3u8`，約 6 分 22 秒。這是分章實錄，不是一鏡到底或已合併的 MP4；章節 14 連續記錄同一 session 恢復至完整季度完成。先前其他研究的技術驗證影片獨立保留，不混入本次清單。

- 使用原 React，錄下同一資料集／研究／trial／session 的可追溯操作：匯入 → 清理預覽及保存 → 工程設定 → 辨識與准入 → Autoresearch → 候選選擇 → 新數位孿生執行 → 完成與比較。
- 顯示 CV／MV／DV 映射與單位、gain／tau1／tau2／dead time、實際搜尋參數與 trial ID；模型參數和 MPC tuning 分開說明。
- 閉環片段必須包含原始初值、單步、連續推進、暫停與恢復，並看到實際回傳的 MV／CV／DV、模擬時間及掃描數變化。暫停不跳掃描，變速不改 sample_time。
- 完成後顯示全期筆數、基準／候選比較、CV 變異與偏差、限制違規及 MV 動作代價；不得只挑改善片段或用縮短情境代替完整季度驗證。
- 保留原始實錄；若製作精簡版或加速等待區段，清楚標記剪輯與倍速。說明附章節時間、資料來源與 run/session 身分，不把數值預測說成工廠實測。
- 交付前實際檢查影片可播放、字體可辨、關鍵操作與完成狀態均有畫面，以及影片數值與封存結果一致。登入密碼、CSRF token、DSN 與其他秘密不出現在錄影中。

影片研究由 `4640a132d3ad47d1822b5e5b6e25ebdf` 更正漏填的 FeedMoisture 工程上限，經重新確認保存為 `365e1f73ef604f37ad0e9410db76208e`，原版保留。清理資料為 `ds_58acedef774042ecb7230c3f8a9e74fd`；run `f3e40f615fe54aeb9693478031c23838` 已完成 95／95 工作及 16 項最終檢查。129,600 筆、六輪、26 個候選、兩方法、三情境及原准入門檻均未縮減。標準 DMC 與 Soft-band 的完整季度 Direct／COM 批次各有九個 trace 欄位逐筆相同，forecast SHA-256 相同，bridge 正常退出；未再出現先前 SIGKILL，但沒有宣稱已定位或修復其根因。

本案例保存標準 DMC trial-011 為 `review_only` 版本 `31627e87e5a5ed9e048e235aee39457247c02b55b286835cc530e1d90afd2eb6`。新 Direct session `173c6da7447047c5835ba1f0127021c8` 完成 129,600 筆；控制器 hash 與該審查模型相同，九個 trace 欄位及完整預測封存與同 run 原生批次一致。留出改善、MV 代價與方法比較見 `VIDEO_GUIDE.md`；這仍是合成歷史所辨識之模型上的閉環證據，不是工廠改善或 ABB 投運許可。

續接工具環境中 `127.0.0.1:43180` 已回覆連線拒絕，先前 `/tmp` 啟動腳本亦未保留；不宣稱 listener 現在可用。錄影片段、研究與 session 結果均已落盤。尚未執行選用的影片合併命令；原始 MP4、播放清單及說明可獨立檢視。

## 3D 水泥磨數位孿生看板（設計待實作）

本節回補先前漏交的設備 UI，並依使用者最新要求改為真正的 3D 場景；不能用 2D 卡片、等角插圖或動畫影片代替正式 3D 驗收。本次交付為設計文件與靜態版面預覽，不安裝依賴、不改動運行程式、不宣稱已實作。

設計預覽：[`cement-mill-3d-dashboard.html`](hmi/frontend/design/cement-mill-3d-dashboard.html)；[桌面設計圖](hmi/frontend/design/cement-mill-3d-dashboard-desktop.png)。其設備為閉路球磨示意，不是現場 P&ID；數值與曲線取自既有合成資料閉環封存，並非現場即時資料。預覽中的圖形不是 WebGL 引擎，控制項也不是可操作的產品。

靜態稿已在瀏覽器檢視 1440 px 桌面與 390 px 手機：無整頁橫向溢出，三區的 X 繪圖範圍各自一致，時間標記對齊預測分界，無瀏覽器錯誤。這只驗證設計稿排版；手機產品版的分頁／子泳道、3D 互動與資料驅動動畫仍須實作驗收，不用本頁替代。

### 設計決策與現況差異

| 項目 | 已有能力 | 本次必須補上的能力 |
| --- | --- | --- |
| 設備畫面 | `DigitalTwinProcess` 的角色卡片與模型拓撲 | 原 React 內完整 3D 設備、連線、點位標籤、選取與動畫 |
| 模擬閉環 | 原生 Rust session，MPC → MV → 製程 → CV | 場景訂閱同一工作階段，不另寫模擬器或假資料迴圈 |
| 點位顯示 | frame／recent trace 的 CV、MV、DV、命令及品質 | 明確的設備綁定、來源／時鐘／品質／資料年齡、雙向選取 |
| 設定 | 研究設計與凍結候選設定 | 看板側邊的角色設定入口、版本差異與生效範圍 |
| 三區圖 | `ProcessTrendChart` 的 `role_blocks`、選線與獨立軸 | 與設備及面板同步、目前時間線、最新完整預測時域 |
| 預測 | 封存 `forecast.ndjson` 有完整時域；`TwinFrame.predicted_cv` 只有單步向量 | 原生快照提供同一次 solve 的完整、有限大小 forecast envelope；現場路徑補齊時間與模型身分 |
| 視覺技術 | React 19／TypeScript／Recharts，尚無 Three.js 依賴 | 延遲載入 Three.js＋React Three Fiber；相機與互動以必要元件為限 |

**產品定位**：這是控制用的 3D 製程看板，不是 CAD 編輯器，也不是由 3D 外形推算物理的模擬器。SOPDT／FIR 仍只代表已辨識的輸入輸出動態；3D 外形、物料粒子及轉動不增加模型保真度，也不構成節能或工廠改善證據。

### 一個看板，不再藏操作入口

- 入口沿用原 React 的「Autoresearch → 數位孿生」。沒有 session 時仍呈現設備配置與未取得資料狀態，不再讓主要操作藏在另一個模式下。
- 頂部固定顯示：資料模式、研究／方法／trial／模型版本、情境與傳輸、目前時間、求解／品質狀態，以及建立／單步／連續推進／暫停／重跑。
- 桌面：上方主要區域是 3D 設備，右側為點位與 CV／MV／DV 設定，下方是三區趨勢。3D 與趨勢可各自放大，但不能讓暫停、模式或資料來源消失。
- 點選設備 → 右側列出該設備點位；點選點位 → 聚焦對應角色與曲線。反向點選曲線或面板變數 → 場景高亮設備。選取不改變控制值，不偷偷隱藏其他角色。
- 主要畫面只保留值、單位、品質、偏差與必要操作。node ID、模型矩陣、雜湊、准入證據、參數解釋收進「工程細節」，不再以長段文字佔滿操作區。
- 小螢幕使用「設備／趨勢／設定」分頁，保留頂部來源與控制列。趨勢仍按 CV／MV／DV 分區，每變數自己的子泳道；不把所有 Y 軸擠進一張小圖。
- 場景提供旋轉、縮放、平移、點選、回到全廠與聚焦設備。相機只有使用者操作或明確聚焦才移動，不因每次資料更新自行飛行。

### 模式與操作語意

| 模式 | 時鐘與資料來源 | 允許的主要操作 |
| --- | --- | --- |
| 閉環模擬 | session 的模擬時間與原生求解結果 | 建立凍結模型執行、單步、連續推進、暫停、完成後重跑 |
| 封存回放 | 所選 trial／session 的封存游標 | 播放紀錄、拖曳時間、比較；不產生新 CV，也不寫入設備 |
| 現場觀察 | 實際 OPC 量測及其來源時間／接收時間 | 觀察、選線、經授權的既有設定流程；不因開啟看板自動 Engage |

隔離 research listener 不得因看板切換而開啟現場連線。現場模式只在部署配置允許時可用，否則顯示不可用原因。切換來源時清空舊來源的點位與曲線，再載入新來源；不混用 UTC、相對時間、封存游標與瀏覽器牆鐘。

「開始模擬」在無 session 時先建立一次，再以回覆的身分和序號推進；建立結果未知時不自動重送。已有暫停 session 則續跑原 session。完成後的「以此版本重跑」建立新的 ID，不覆寫原結果。暫停立即停止排送下一批，但顯示「本批完成後暫停」直到已發送的有限批次結束；不能假稱已撤回原生運算。

即時更新與牆鐘等速分開：既有 session 在有限批次完成後回傳 frame，畫面顯示最近已確認的 sample 與「本批計算中」。逐步觀察可用每次一掃描；完整季度可用較大批次，但每個原生掃描仍完整求解。不能用 JavaScript 計時器生成中間 CV，或把視覺插補當成已收到的新量測。

### 設備場景與資料綁定

閉路球磨的設計示意包含原料倉、定量給料／輸送、磨機、提升機、選粉機、粗粉回料、收塵器、風機、成品輸送／料倉及相應物料／氣流連線。設備型式、數量、位置與管路以經確認的設備配置為準；目前未取得現場型式，不能從 `MillDifferentialPressure` 等名稱推定是球磨或立磨。立磨不能套用錯誤的球磨外形與連線。

- 使用可配置的真實 3D 幾何元件；可用 TypeScript 程序式幾何，或經確認、隨產品提供的 GLB。無需引入其他程式語言或外部模型服務。本次不要求編輯 CAD。
- 版本化 `equipment view` 保存設備 ID、型式、位置、連線、標籤錨點、點位綁定與動畫規則；與控制器物理／調參模型分離，不把場景資料塞進 `UnifiedModel.physics`。
- 點位以凍結控制器身分、角色與精確原始變數名稱定位，校驗順序、維度與 `units`；現場再使用模型中的精確 `node_ids`。不做大小寫猜測、名稱包含比對或缺失節點補值。
- 綁定通道明確區分 CV PV／target、MV applied-SP／SP register／command／PID OP，以及 DV PV。PID OP 有獨立單位，不冒充 MV 的工程量或應用回授。
- 設備額外電流、溫度、運轉／故障訊號需要明確配置來源與單位；沒有來源就顯示「未綁定／未建模」，不能顯示 0、正常或隨機動畫值。
- 既有六變數範例只涵蓋 ProductFineness、MillDifferentialPressure、FreshFeed、SeparatorSpeed、ClinkerHardness、FeedMoisture。將它們放在設備上的位置屬於設計綁定，不代表既有模擬器已建模每台設備。
- 新 session 凍結 equipment-view 版本／雜湊；更新設備綁定不回寫已完成 session。舊 session 無此資料時明示「設備配置未封存」，不得冒充當時配置。

### 動畫是資料的呈現，不是另一套數據

| 動畫 | 合法驅動來源 | 缺值／非運行狀態 |
| --- | --- | --- |
| 選粉機轉子 | 已綁定實際／模擬轉速 | 缺失或過期即停止，顯示未知；轉速視覺縮放不冒充實際 RPM |
| 給料與物料粒子 | 對應流量及方向；只對有資料的路徑動態呈現 | 未建模的回料量不能由 FreshFeed 假造；只保留靜態流程方向 |
| 磨筒／皮帶／風機 | 現場需其運轉／速度點；模擬可用明示的情境運轉狀態 | 模擬情境狀態不是設備量測；不得移植到現場當運轉證據 |
| 設備告警提示 | 該設備明確品質／狀態來源 | 不靠 3D 顏色推斷物理故障，不以閃爍取代告警文字 |

數值與趨勢只在收到新資料時更新；視覺幀率與採樣頻率分離。插補僅平滑幾何動作，不新增可供 tooltip、統計或控制使用的樣本。模擬暫停／失敗／斷線、封存播放暫停時凍結流程動畫；失聯保留最後值並標示年齡與失聯，不能看起來仍然正常流動。支援降低動畫與關閉粒子；相機仍可操作。

### Realtime 點位卡

每個點位卡含角色、短名稱、數值與單位、來源、時間、品質；展開才顯示原始 tag、品質碼與版本。來源必須區分現場量測、模擬輸出、MPC 命令、預測與情境輸入。模擬運行可即時更新，但標題必須寫「模擬即時」，不可寫「工廠 Live」。

品質統一成 good／uncertain／bad／missing，但保留原始編碼類型；OPC DA 的 192 與 OPC UA 的 0 不能混用解析。資料年齡根據來源時鐘、更新間隔及明確的 stale policy 判斷；只有 HMI 接收時間時標示「接收時間」，不冒稱 PLC source timestamp。時間倒退、來源切換、序號不一致或品質不良形成可見斷點，不用向前填補掩蓋現場失聯。

同一看板數值、動畫與趨勢必須對應同一來源及已接受的 frame。載入中的新來源不得暫時套上舊來源的 Good 狀態；預測另驗證 solver 狀態、發布時刻與模型版本，不因實際值 Good 就自動有效。

### CV／MV／DV 設定面板

| 面板 | 日常欄位 | 工程欄位 |
| --- | --- | --- |
| CV | 變數／設備、單位、目前值、target、操作上下限 | 規格來源、weight、alpha、optimization_mode、slack_weight、is_integrating、精確 node IDs |
| MV | 變數／設備、實際值與命令、上下限、每掃描 max_move | weight_r、選填經濟 target／target_weight、applied-SP 回授、PID OP、mode／mode_target、精確 node IDs |
| DV | 變數／設備、目前值、輸入來源、量測有效範圍 | PV node、情境來源與變更；DV 不是可由 MPC 下命令的 MV |

- 先選資料來源，再選變數／設備及角色；匯入時保留物理 ID。單位、同時重複角色、缺失 node、上下限次序與模型維度錯誤在儲存前阻擋。
- 面板明示「檢視凍結版本」或「編輯草稿」。改動角色／映射／模型／權重／限制／DV 情境時顯示差異及影響；保存新版本並建立新研究或 session，不熱改舊 trial。改變情境後的比較必須重新配對基準。
- target、操作上下限、產品規格、量測有效性界限是不同語意。沒有規格資料就顯示未設定；不把 `limits.low/high` 自動改名為產品規格。
- 「儲存研究版本」不等於「寫入現場」。現場支援的設定寫入沿用登入／角色／CSRF 與 `writeOpcNode` 的 checked acceptance，顯示待確認、伺服器已接受、結果未知；實際 PID 應用仍是另一個 gate。
- 不因看板動畫、值相同或 WebSocket 更新就報寫入成功。Engage、arming、RCAS、資料新鮮度及應用確認仍由現有控制權流程把關，場景不得直接寫 OPC 或繞過主管控制器。

### 三合一即時與預測趨勢

沿用 `ProcessTrendData`／`ProcessTrendChart`，不新增第二套曲線實作。固定 CV 在上、MV 在中、DV 在下，共用時間範圍、游標、縮放與相同 X 像素位置；每一變數仍有自己的工程 Y 軸。兩變數可用左右軸，更多變數或窄螢幕採角色內子泳道。兩個候選的同一變數可以共軸，不同變數不共尺度。

| 區塊 | 預設顯示 | 可勾選 |
| --- | --- | --- |
| CV | 實際值、目標、所選發佈時刻的最新完整預測 | 歷史固定 lead 預測、產品規格、操作／soft-band 界限、另一候選 |
| MV | 已應用工程值或明示較弱的 SP register 回讀、MPC 命令 | 未來控制計畫、操作限制；PID OP 另置獨立子泳道 |
| DV | 實際擾動值 | 已知未來情境／具來源的擾動預報、有效性界限 |

實際值用實線，預測／計畫用虛線，限制帶淡色且可隱藏；有清楚圖例，不只靠顏色識別。每個角色保留選線與軸範圍按鈕，預設不疊滿全部歷史 forecast。未來區域以目前時間線分隔；歷史預測按 valid time 對齊，不能畫在 issued time 上冒充沒有延遲。

完整預測資料須包含發佈時刻、sample_time、prediction／control horizon、首步 offset、排列方式、CV 向量、MV base／增量、來源與模型身分。延用既有封存的 step-major 契約：CV 首步 offset=1，MV 計畫首步 offset=0；valid time 依欄位計算，不猜測。MV 累積增量產生的是控制計畫，不是保證會被執行的實際值。

原生 session 回覆增加有界的 latest forecast，直接來自該次求解；snapshot 不重算、不推進。HMI 與前端只傳遞和呈現。現場引擎的 forecast 亦需在同一次 solve 發佈相干 envelope；只收到沒有發布時間的舊向量時標示不可用，不能拿 WebSocket 接收時刻補造。

DV 未來值只有在情境或外部預報確實提供時才顯示，並標示來源；否則未來區域留空。沒有經校準的不確定性資料就不畫置信帶。未來 actual 不補值，solver 失敗不沿用舊 forecast 冒充新解。

績效卡使用相同來源與時間窗的完整解析度統計，顯示 CV 標準差／變異、目標偏差、越限與 MV 動作代價；不從動畫、顯示減點或不同情境的曲線算改善率。

### 模組與資料邊界

- `DigitalTwin.tsx`：保留研究與 trial 選擇，組合看板；統一選中來源／設備／變數／時間，避免每個子元件各自輪詢。
- `DigitalTwinSession.tsx`：仍獨占序列推進與不確定結果處理；把操作移到可見工具列。場景不持有第二個 advance timer。
- 新 `DigitalTwinScene`：僅接收已驗證的 equipment view、frame、選取狀態與顯示設定，輸出選取事件；不執行控制算法、不取資料、不寫設備。
- 新點位／設定面板：使用同一變數契約；版本保存走既有研究／模型流程，現場寫入走既有 checked-write 流程。
- `DigitalTwinModel`：辨識、SOPDT／FIR 及雜湊證據保留為可展開工程資訊。舊角色卡片不再充作設備主畫面。
- `ProcessTrendChart`：繼續擁有三區圖、獨立軸、選線、品質斷點與比較；場景只傳選取／時間，不複製 chart 狀態機。
- 原生 Rust session／engine：提供同次 solve 的完整 forecast；保存與快照共用輸出，不在 TypeScript 或既有 orchestration 另算 MPC。
- 後端 equipment-view 介面：讀寫採既有身份／權限／CSRF；伺服器驗證模型相容性後保存版本，session 只接收版本 ID 並由伺服器解析。禁止瀏覽器指定檔案路徑、任意外部資產 URL 或現場 endpoint。

繼續使用專案既有程式語言；新 3D 元件是 React／TypeScript，控制資料契約修改落在既有 Rust owning layer，不引入第二套控制引擎或外部服務。Three.js／React Three Fiber 為待新增依賴，尚未安裝。

### 效能與操作退化

3D 模組延遲載入，避免增肥非孿生頁面的初始 bundle。相機、轉子及粒子用渲染層更新，不每幀更新整棵 React tree；相同粒子／設備重用幾何與材質，數據緩衝有界，背景分頁停止無效渲染。先以完整場景及實際點位數量量測，再決定幾何與標籤密度，不能以刪設備降低驗收範圍。

效能驗收目標為約定測試桌機／瀏覽器上的可持續 30 FPS 與不卡住的主要操作；硬體、場景規模及觀測結果必須記錄，這不是目前已達到的數字。WebGL 失敗時明示 3D 不可用，保留可操作的點位表與趨勢以利診斷；此退化畫面不算 3D 驗收通過。提供鍵盤可達的設備清單、完整單位、非顏色狀態提示及 reduced-motion 模式。

### 實作順序與端到端驗收

1. 先確認設備型式／清單／連線與點位綁定，完成版本契約及完整 forecast 的原生資料路徑；不先做漂亮假值動畫。
2. 在原 React 整合真正 3D 場景、固定操作列、點位選取與設定面板；完成場景／面板／三區趨勢的雙向選取。
3. 接上資料驅動動畫、完整預測與各來源狀態；處理零筆、暫停、未知命令結果、斷線、品質不良、錯誤模型與完成重開。
4. 由原 React 重新操作 CSV／PostgreSQL 已有匯入入口、清理、辨識、Autoresearch、選版及新完整季度 session；驗證所選模型／設備配置／情境身分。未授權來源不可為了拍影片連線；可用資料來源須各自有獨立取得與清理證據。
5. 執行完整 129,600 掃描，保留原閉環數值等價、Direct／隔離 COM 路徑及不可變原始 trial 要求；3D 不改變原生 CV／MV／DV／command／forecast。不能用短程 smoke、預錄動畫或固定數值代替。
6. 逐項驗證：全廠設備皆可定位；有來源的動畫跟隨數據、無來源保持未知；點選一致；設定差異與生效範圍明確；未經同意不改舊 session、不寫現場。
7. 驗證完整預測來自同次 solve、時間 offset 正確、模型身分一致、MV 計畫不當實際、DV 不造預測；每變數獨立 Y 軸，三區同一 X 位置，桌面與手機選線不丟失。
8. 實際原 React 量測 3D 效能、背景／暫停／失聯行為、鍵盤操作與瀏覽器錯誤；錄影從開啟看板、設定／選模、啟動、動態閉環、暫停恢復到完整完成比較。新影片須明確包含 3D 設備與動畫，舊卡片影片不能代替。

**完成判準**：數值閉環、操作流程與 3D 設備 UI 各自取得證據後，才能宣告整個數位孿生看板完成。現有數值與影片證據保留，不將其追認為尚未實作的設備功能；真實 ABB／現場控制仍需獨立授權與投運驗證。

## 設備專家工作區重新設計提案（待討論）

使用者最新要求先提出整體 UI 與持續自行優化專家系統方案。前節 3D 工作已有開發中變更，但尚未完成整合、建置與新一輪完整驗收；不得將程式片段、提案預覽或舊工作台影片當成本次產品已完成的證據。既有實作保留，待新工作區範圍對齊後續接，不降低前節完整季度、真實 3D、資料來源與影片驗收標準。

新版面提案：[`expert-twin-workspace.html`](hmi/frontend/design/expert-twin-workspace.html)。這是可切換七個流程說明面板的靜態設計原型，不是 WebGL 場景、不連現場、不執行訓練或控制。未取得的數值顯示缺值，不生成模擬績效。已在瀏覽器檢查七個面板切換；1440 px 與 390 px 的整頁 scrollWidth 均等於 viewport，未觀察到頁面 JavaScript 錯誤。此檢查僅適用提案。

### 提案核心決策

- 設備孿生升為主工作區；設備負責視覺與點位導航，控制群組負責耦合的 CV／MV／DV。每個 MV 同時只有一個有效命令擁有者。
- CSV／PostgreSQL 歷史與 OPC 現場採集共用精確訊號契約，但畫面來源、時鐘與控制權分開；統一 React 入口不移除研究與現場後端的隔離。
- 模型辨識與 MPC 調參分層。FIR、FOPDT、SOPDT 保存原始模型與適用工況；提供 DMC 的響應矩陣須具有明確取樣、輸入座標、完整 MV／DV 路徑及轉換證據。
- 快閉環由已核准版本持續讀取有效資料、求解與寫入；慢閉環依新資料、工況與漂移觸發清洗、辨識、調參及模擬。研究失敗不得拖慢控制掃描。
- 模擬排名第一只是候選。留出驗證、工況資格、資料品質、現場授權及交接條件通過後才可發布；沒有合格改善就維持現行版本。
- ABB 若有核准的 UA 路徑優先沿用；僅有 DA 時建議獨立 Windows DA→UA Gateway。現有隔離同步 COM fixture 不當作生產連接器，也不構成遠端 DCOM／ABB 驗收。
- 自動化依序為建議、核准後持續控制、有界自動升版。最後一級須另有預先核准政策、觀察期、切換冷卻、狀態初始化、無擾動交接及回退契約；本提案不授予現場寫入權。
- 專家判斷補上資料／感測器故障與模型漂移區分、閉環辨識偏差與激勵檢查、慢速化驗與軟感測來源、產品與負荷工況，以及產量／品質／MV 動作代價。不能只最小化 CV 變異，也不能將同資料推導的 twin 當獨立工廠真值。

### 多製程 3D 工作區與 Runtime／建模隔離（新增設計要求）

使用者明確要求：水泥磨、生料磨、旋窯、煤磨都使用設備工作區，且正式主視覺為真正 3D；Runtime 與建模須分開，避免誤操作。前述統一工作區是共用產品入口與呈現能力，不再把研究流程及現場控制放在同一組操作面板。

#### 製程範圍與共用契約

- 導航層級為工廠 → 產線 → 製程區域／設備實例 → 控制群組。四種製程都可有多台實例，不以四個固定名稱取代設備 ID。
- 水泥磨的球磨／立磨／輥壓機組合、生料磨的研磨／乾燥／均化、旋窯的預熱／分解／窯體／冷卻、煤磨的給料／研磨／收塵／煤粉倉，分別配置設備場景、拓撲、點位、模型與工況；上述是配置範圍，不是已確認的現場 P&ID。
- 共用 React／TypeScript 3D renderer、設備選取、品質顯示、三區趨勢及版本介面；不共用可變控制狀態，不將水泥磨模型或 Tag 複製為其他製程的合格設定。
- 3D 幾何不等於製程物理。各製程的模擬模型須各自辨識與資格驗證；對未支援的積分、強非線性、時變或多速率量測，明確顯示方法限制，不強行套用穩態 FOPDT／SOPDT。
- 跨區域共享量測可以唯讀引用；共享 MV 的命令擁有權按精確物理目標全域檢查，不僅在單一畫面內檢查。同一 MV 不可被兩個活躍控制群組同時接管。
- 旋窯燃燒安全、煤磨防火防爆／惰化及設備聯鎖保留在經核准的 DCS／SIS 或既有安全保護系統；MPC 研究與自動調參不得修改或繞過。

#### 兩個工作區，不是兩種控制模式

| 項目 | Runtime：運行監控 | 建模：隔離研究 |
| --- | --- | --- |
| 主要資料 | 經核准現場採集、已安裝版本、實際控制狀態 | CSV／PostgreSQL 快照、採集端唯讀輸出、清洗資料、模擬 session |
| 3D 主視覺 | 已核准設備配置，真實量測與具來源的預測；缺值及失聯明示 | 草稿／凍結設備配置，歷史回放或隔離模擬；永久標示非現場 |
| CV／MV／DV 面板 | 角色與映射唯讀；僅具權限者可沿既有流程修改允許的操作設定 | 編輯角色、映射、模型與研究草稿，保存新版本，不熱改現場 |
| 可用操作 | 監視、查看品質與告警、核准版本交接，以及獨立授權的控制操作 | 匯入、清洗、辨識、調參、模擬、比較、候選送審 |
| 版本生效 | 審查／核准、安裝、啟動／接管分開；需當前控制契約校驗 | 送審不代表核准，更不代表安裝或現場生效 |
| OPC 寫入 | 只由受控 runtime 執行，受角色、狀態及逐項結果約束 | 無現場寫入連線或憑證；模擬命令只能到隔離 plant |

進入 Runtime 預設只讀，不會自動 Engage。Runtime／建模是工作區模式，與控制器 Off／Monitor／Engage 及故障 Hold 狀態分開呈現。更換工作區、製程或設備是導航，不啟動、停止、解除保護或重新接管控制器；運行中的控制器可在背景繼續，畫面須明示其最後確認狀態及時間，狀態未知不能顯示已停止。

#### 必須由後端落實的防誤動作

1. 正式版採獨立工作區路由與後端執行環境。可共用 React 程式碼，但研究環境不持有現場寫入能力；正式部署以隔離的前端來源／會話範圍與服務權限避免建模頁取得 Runtime 寫入憑證。只隱藏按鈕、顏色區分或前端布林值不構成安全隔離。
2. 建模讀取現場資料時，僅消費採集端輸出的唯讀紀錄／快照；不從研究設定接受任意現場 endpoint、DSN 或可寫連線。模擬／回放／現場的資料訂閱、時鐘與命令佇列分開。
3. 切換工作區或設備時，清除未送出的操作表單、舊選取及舊資料訂閱；不將研究草稿、模擬值或待送命令帶入 Runtime。未保存研究草稿須明確選擇保存或放棄。已送出的控制請求仍按原目標追蹤至已接受／失敗／結果未知，不因切頁假稱取消。
4. 寫入須帶精確工廠／設備／控制群組／物理 Tag、預期版本與操作意圖；後端核對目前權限、控制擁有權及狀態。拒絕過期頁面、跨製程映射、版本已變更及競爭命令；同一操作 ID 的重試只查詢／回傳原結果，不重新執行。不以「目前選中設備」隱式決定寫入目的地。
5. 高影響操作另顯示操作對象、目前／新值、單位、作用版本與預期結果，取得明確確認；角色／模型／權重等研究修改不能因按下保存直接寫入現場。
6. 自動升版只能由獨立受控發布流程執行，遵循已核准的政策與交接條件；研究程序不能自授權，工作區切換不觸發升版。

#### 新增驗收

- 四製程、多實例與兩工作區切換後，設備、來源、版本、選取及圖表不得串線；後到的舊請求不能更新新工作區。
- 建模環境即使直接呼叫寫入路徑也無法控制現場；Runtime 唯讀使用者不可因 UI 模式或角色名稱取得寫入權。多分頁、過期表單、競爭控制器與部分寫入均須驗證。
- 切換到建模不停止已運行控制器；切回 Runtime 不啟動控制器或恢復 Engage；未知執行結果持續可追查。
- 四製程各自以實際 3D 場景、點位品質、資料驅動動畫及三區趨勢驗收。單一水泥磨的成功、靜態設備草圖或單純更換名稱不代表其他製程已完成。

提案預覽已更新為 Runtime／建模兩個操作入口與四個製程配置草圖。瀏覽器實際操作 1440 px／390 px × 兩模式 × 四製程，共 16 組導航狀態；各組僅呈現對應製程草圖、對應模式導覽及來源標示，未觀察到整頁橫向溢出。另逐一操作七個建模面板及三個 Runtime 面板，未觀察到頁面 JavaScript 錯誤。這些是提案互動檢查，不證明正式路由、後端權限隔離、WebGL 3D 或現場安全已實作。

## 逐頁功能規格與可審查契約（設計，非產品驗收）

### 設計立場

3D 是設備理解與空間定位工具，不是每頁都必須佔滿的背景。設備總覽、設備綁定與閉環模擬預設展開；清洗、辨識、調參、績效、版本審查以資料為主，3D 預設收合。每頁都有自己的功能內容、有效操作及結果，不再只替換側邊說明。正式產品使用真正 3D；設計原型的設備草圖不計入 3D 驗收。

「任何 reviewer 都找不出問題」不是可保證的驗收條件。本規格提供逐頁輸入／輸出、准入條件、失效語意、權限矩陣及可重現驗收案例，供控制、製程、資料、資安及 HMI 專家逐項挑戰。未取得真實設備、ABB 契約與安全審查前，不宣稱投運合格。

### 共用工作區外框與 3D 行為

- **始終可見的最小狀態列**：工作區模式、工廠／產線／製程／控制群組、來源與時鐘、作用版本、控制狀態及資料年齡。建模顯示其研究／run／trial／session；現場狀態若另外展示，獨立標為唯讀，不能混入模擬數值。
- **導航不是 wizard gate**：Runtime 7 頁、建模 9 頁可直接進入；缺少前置產物時顯示缺什麼與返回入口，不隱藏整頁。可檢視舊結果，但不把它當作目前草稿的有效結果。
- **3D 控制三態**：「展開」「收合」「聚焦設備」。收合只保留一列設備／選取摘要與「顯示設備」按鈕，不留空白欄位；功能區取得釋出的空間。此設定是呈現偏好，不是資料來源或控制狀態。
- **狀態不丟失**：同一設備配置版本內，收合保留選中設備、相機、趨勢選線／游標、表單草稿及執行中工作。偏好按使用者／工作區／頁面／斷點記錄，避免手機偏好覆蓋桌面；相機按設備配置 ID 記錄，跨製程或版本不套用舊相機／選取。
- **顯示資源與控制分離**：收合、背景分頁或 reduced-motion 停止無需的 GPU 動畫；不停止資料採集、控制器、研究或模擬 job。長時間釋放 WebGL 資源後，重新展開以最新已確認 frame 重建，不能從隱藏前的舊資料開始播放。
- **選取替代路徑**：3D 隱藏時仍可用設備樹、點位表及圖例選取。點選曲線可提供「在設備中定位」，但不突然強制展開或移動相機。告警也不自動打開 3D。
- **手機**：同頁功能內容優先，3D 為可開啟區塊；不是縮小全部桌面欄位。大表格在區塊內捲動，跨頁保存條件；模式、對象、資料時效與危險操作確認不可因螢幕縮小而省略。
- **無障礙**：所有設備及操作可鍵盤到達；收合後焦點移至展開按鈕；不依顏色判斷品質／模式。資料更新不搶焦點、不反覆朗讀每筆量測；重要狀態與使用者操作結果才使用 live region。

### Runtime 頁面

#### R1 設備總覽｜3D 預設展開

- **問題**：現在是誰在控制、哪些設備或資料有問題、應先看哪裡？
- **內容**：3D 設備與流程、具品質與時刻的重點點位、控制群組清單、最需處理的事件；下方為選定群組的 CV／MV／DV 三區趨勢。摘要包含 CV 偏差／越限、MV 飽和／應用回授、求解與資料健康；沒有有效視窗就不顯示績效數字。
- **操作與輸出**：選設備／點位、聚焦、選線、前往 R3 控制操作或 R4 事件；本頁不放快速寫入、Engage 或模型切換按鈕，選取不產生命令。
- **空態／故障**：未配置設備可看點位表並提示配置入口；斷線保留最後值但明示最後確認時刻／未知，動畫停止；無預測與無 DV 不畫零線。

#### R2 趨勢與診斷｜3D 預設收合

- **內容**：時間窗與時區、Live 跟隨／歷史游標開關、變數與逐條選線；三區共用 X 軸、每變數獨立 Y 軸。CV 實際／目標／預測／規格／操作限分開；MV 命令、已應用 SP 與 OP 分開；DV 不虛構預測。
- **診斷區**：預測發出時間、valid time、lead、控制版本；精確對時且同方法的殘差、資料缺口、版本／目標／模式事件。顯示取樣頻率與展示抽樣；圖上降採樣不改統計。
- **操作與輸出**：游標、縮放、工程尺度、保存檢視、核准範圍內匯出帶來源的資料。拖曳歷史視窗立即退出 Live 跟隨；返回現在需明確操作，不用新資料把使用者拉回。
- **空態／故障**：部分訊號缺失可看其餘訊號，但缺失明示；超出查詢上限提出縮小時間窗，不以截斷結果冒充完整。時間／版本不相容時不疊加預測。

#### R3 控制操作｜3D 預設收合，可聚焦

- **內容**：現在／請求模式、唯一命令擁有者、已安裝模型、採樣與求解、arming／PID 實際及請求模式／資料新鮮度／應用確認條件。CV 的允許操作目標／界限、MV 命令／回授／限幅、DV 唯讀；顯示修改前後值、單位與作用範圍。
- **操作與輸出**：具 `runtime.operate` 權限者才可提出經允許的設定或狀態變更；結構、映射、方法與凍結調參不在此編輯。每次提交固定對象、預期版本／控制 epoch、操作 ID、有效期與原因，後端重驗。
- **MV 互斥**：本版 R3 不提供手動 MV SP 直寫；MV 命令／回授始終唯讀，Engage 期間更不得由 HMI 與 MPC 搶寫。人工 MV 干預須先依已核准交接流程退出 MPC，確認命令擁有權已釋放，再由 DCS 的既有操作介面處理。操作限只能在 Runtime 核准安全包絡內調整，不能順便放寬硬限。
- **結果狀態**：待送出 → 執行中 → 伺服器接受／拒絕／結果未知；另外呈現應用確認／逾時。值相同、WebSocket 更新或逾時均不當成功；結果未知先查原操作，不以新 ID 盲目重送。
- **失效與退出**：Hold、停止新命令、正常退出 MPC、DCS／PID 實際接手是不同狀態。頁面不提供替代實體緊急停機的「安全急停」假象；重新接管須重新驗證，不能以解鎖畫面清除控制保護。
- **交接握手**：Runtime 按設備 DeploymentProfile 指定的退出／watchdog／DCS 接手契約執行，只有已核准的模式／釋放 Tag 才可寫入；不假設 ABB 數值或強制切到 Auto／Cascade。狀態分開顯示「MPC 已停止新命令」「DCS 接手待確認／已確認／逾時」，逾時／未知立即告警並提示現場處置。缺少該契約時只能觀察，不核准自動控制或切版；Hold 後不得自動重接管。
- **交接未確認互鎖**：DCS 接手待確認／逾時期間禁止重新 arming／Engage。解除須取得 profile 規定的有效現場確認與唯一擁有權證據，再重新核准接管；單純點擊「我已確認」不能替代該證據或解除安全互鎖。

#### R4 事件與資料健康｜3D 預設收合

- **內容**：按嚴重度／設備／時間分類的事件、唯一事件 ID、首次／最後發生、來源、條件是否仍在；品質矩陣、時鐘偏移、資料年齡、命令失敗及應用確認時間線。
- **操作與輸出**：確認「已閱讀」、附註、指派與跳至 R2／R3。應用內確認不冒充 DCS 告警確認，不解除聯鎖、不清除故障、不自動重接管；不支援的外部確認保持唯讀。
- **空態／故障**：事件服務失聯顯示未知，不寫「沒有告警」。同一根因聚合但保留次數與原始事件；事件洪流不阻塞控制 UI，不讓通知反覆搶焦點。

#### R5 績效與漂移｜3D 預設收合

- **內容**：產品／負荷／原料／控制模式的比較分層、有效樣本與覆蓋率、基準來源、CV 實體變異／目標誤差／偏差／違限、MV 動作與飽和；有實際功率及產量才計算單位能耗。顯示統計區間、方法與不確定性來源，不捏造信賴區間。
- **判斷區**：資料／感測器問題、PID 未跟隨、模型殘差漂移與工況外推分開；顯示證據及建議「繼續觀察／收集資料／建立研究」，不只有單一健康分數。
- **操作與輸出**：建立帶精確時間窗／工況／版本的唯讀研究請求，送至 M1；不在此啟動現場試探或改參。現場前後比較不等於因果改善；影子預測不等於反事實閉環真值。
- **空態／故障**：工況不相容、樣本不足、目標改變或資料缺口時不計算改善百分比；不能把停產低變異排名為更好控制。

#### R6 版本、發布與交接｜3D 預設收合

- **內容**：目前／候選不可變版本、來源資料與清洗／模型／tuning／方法／設備／映射／安全契約差異、適用工況、測試證據、審核人與有效期。候選、核准、已安裝、Monitor、已接管狀態分開。
- **操作與輸出**：`runtime.approve` 核准／駁回；`runtime.deploy` 安裝；`runtime.operate` 啟動／接管分開授權。人工發布採作者與核准者分離；admin 不自動跳過審查。核准記錄綁定 candidate hash、plant／line／area／group、設備實例 ID 集合、Runtime DeploymentProfile hash、工況與有效期；任何不相符均須重新核准。
- **Runtime 權威契約**：DeploymentProfile 由現場工程核准，擁有物理 Tag、單位／模式映射、安全硬限、max_move、命令／回授座標及退出策略。研究可以提出工程草稿及引用該 profile 的只讀證據，但候選不能覆寫它；映射或安全包絡修改走獨立變更審查，不能隨調參自動發布。
- **交接計畫**：在安裝／啟動前檢查精確目標、時鐘、初始化歷史、獨立應用回授、無擾動對齊與最後安全狀態；部署交易保存進度，未知結果先核對實際安裝／運行版本。初次 Off／Monitor → Engage 及任何 Hold／斷線後恢復均須操作員重新明確授權。
- **動態狀態重建**：候選影子運行須取得足夠且品質合格的歷史 MV／DV、時序、估測器／偏置狀態，按新模型與其明確輸入座標重建自由響應；不可假設目前 PV 是穩態，也不可直接沿用舊模型的預測狀態。初始化有效期間與誤差門檻依模型／profile 核准，不硬套固定等待掃描數。切換前檢查預測／命令連續性、第一步動作與全部限制；缺少歷史或不支援重建則不切換。
- **回退**：舊版也須符合當前工況與輸入契約，不能按「回退」就無條件熱切；無合格版本時停止新命令並按核准 DCS 交接策略處理。正常頁面導航不是回退。
- **有界自動候選切換**：保留長期自行優化目標，但與自動接管不同。只有現行已授權且健康的 Engage epoch 內，且 Runtime 支援經驗證的無擾動版本交接時，獨立發布器才可依核准政策切換允許範圍內候選；不可跨 Off／Hold 重啟、延長接管授權、改 Tag／硬限／max_move 或自授權。候選先以相同有效輸入完成影子資格與交接檢查，執行時核對 epoch，確保僅一個命令擁有者。能力未支援時顯示不可用，不改成自動停機重啟。
- **政策審查**：人工核准的委任政策須由非研究作者持有 `runtime.approve` 的人員授予，列出精確設備／profile、可變模型／tuning 欄位及範圍、工況、觀察期、冷卻期、最大變化、停止條件與有效期。每個候選仍由獨立驗證器產生綁定政策 hash／candidate hash 的核准記錄。權限／政策撤銷即禁止新的交接；現行控制的退出遵循核准策略，不被瀏覽器登入狀態決定。

#### R7 連線、權限與稽核｜3D 預設收合

- **內容**：管理員已配置的 Gateway／資料來源身分與健康、憑證到期資訊、採集時鐘與映射版本；人員／角色／製程範圍、命令擁有權、操作與版本稽核。一般操作員只看與其職責有關的健康資訊。
- **操作與輸出**：管理員可提出連線／權限變更並看到受影響群組；實際套用需核准的維護／交接條件。密碼、私鑰、DSN 不送瀏覽器、不出現在匯出或錄影；不提供任意網址連線測試或瀏覽器 SQL。
- **失效語意**：登入過期停止新的 UI 變更，不假稱後端控制器已停止；工程師登入身分與 runtime 服務控制權分離。即時撤銷權限由後端落實，過期分頁不得延用。
- **稽核**：記錄誰、何時、哪個來源／版本／設備、舊新值、操作 ID、結果及原因；後端記錄不可由研究工作改寫。保存期限、容量告警、備份與還原政策由部署管理員核准，不能把普通可寫 JSON 當防竄改稽核。

### 建模頁面

#### M1 研究總覽與任務｜3D 預設收合

- **內容**：每個控制群組的資料、工程確認、辨識、搜尋、模擬、留出驗證與候選送審狀態；執行佇列、目前階段、已完成／總工作、失敗與取消原因、來源版本及輸出連結。缺前置產物提供明確下一步。
- **操作與輸出**：建立／複製研究草稿、開啟凍結結果、在能力允許時取消工作；研究草稿用 revision 檢查防止多人覆蓋。伺服器 job 在換頁、登出或關閉瀏覽器後仍可追蹤；重新連線只查詢，不重複啟動。
- **准入**：重跑建立新 run ID、凍結輸入與程式／binary 身分。取消是 cancel-requested 到終止確認，不等於立即停止；部分結果不得標 completed。沒有可恢復 checkpoint 時只允許從頭新跑，不假稱續算。

#### M2 資料來源與快照｜3D 預設收合

- **內容**：CSV 檔案／編碼／分隔符／欄位預覽；PostgreSQL 白名單來源與有時區的半開時間窗；現場採集端提供的唯讀紀錄／快照。來源、取樣與報告時間、品質編碼、NULL、單位、筆數／容量限制、內容雜湊與授權來源均保留。
- **操作與輸出**：預覽及匯入不可變 raw dataset；匯入中顯示階段與可追蹤 ID。超限、截斷、來源變更或讀取失敗整批失敗，不留下可被研究採用的半份資料。
- **邊界**：不是現場連線配置頁，不接受任意 DSN／SQL／OPC 寫入目標；現場快照輸出不可讓高成本研究查詢拖慢控制採集。重複內容顯示既有來源，不悄悄覆寫。

#### M3 設備、點位與工程契約｜3D 預設展開

- **內容**：製程型式／設備實例／連線、控制群組、變數表。每列包括設備 ID、精確原始 Tag／欄位、角色、單位／換算、SP／applied-SP／OP 座標、品質格式、來源時鐘、取樣時間及已知量測延遲。
- **慢速量測契約**：每個 CV 指明是快量測、延遲化驗、或估測值。延遲化驗保存取樣／可取得時刻及品質，不以 forward-fill 當新鮮快掃描回授。要用於快閉環必須另有合格且因果的 estimator artifact，記錄估測版本、輸入／延遲補償、初始化、最大有效年齡、偏置更新、誤差與失效回退；未具備者只供品質監督／驗證，不冒充可用控制 CV。這是需實作的能力契約，不預設所有製程適用某一固定觀測器。
- **工程區**：CV 目標、產品規格與操作限；MV 上下限／每掃描 max_move／模式與回授；DV 量測有效範圍。安全硬限、產品規格、操作限與觀測資料範圍不得混為同一欄位；顯示來源與審核人。
- **操作與輸出**：以設備樹或 3D 選設備後配置點位、檢查耦合／角色、保存工程與設備版本。可提出角色建議但需工程確認；缺失／不明／重複物理映射不能自動補值。跨區域共享 MV 顯示衝突。
- **共享致動器責任**：DeploymentProfile 指定固定主控群組；其他群組以適合其模型的唯讀 DV／協同量引用，不靠「誰先 Engage 誰取得」分配。主控 Hold／退出不讓其他群組自動搶占。改變主控是獨立工程交接與映射版本變更，須兩側停止派送、確認現場擁有權與重新核准；本版不提供臨時搶鎖按鈕。
- **空態／故障**：未知設備型式保持未確認，不猜球磨／立磨。未綁定設備保持靜態／未知。未確認工程契約只能存草稿，不允許後續模型宣稱可投運。

#### M4 清洗與工況整理｜3D 預設收合

- **內容**：原始／清洗後對照曲線、差異與品質遮罩、缺口／重複／離群計數；區間刷選、產品／負荷／停機／換料標籤；時間校正、去重、重採樣、有限填補與單位換算規則。
- **防洩漏**：先凍結 train／validation／test 時間切分與清洗規則；需要估計的閾值只用 training 資料。辨識所需歷史初始化／分割間隔與 causal history 明示；不能用未來化驗報告或跨切分插值補當時資料。
- **保留資料曝光**：M2／M4 的 raw 預覽、工況刷選、品質排除及匯出也納入 test 曝光紀錄。預先封存的 test outcome 在模型／規則凍結前遮蔽；自動品質檢查只用已凍結規則，不能依 test 結果手動調規則。已被探索、外部已知或用於調整的區間只能標 exploratory，不能再宣稱 untouched；需要另保留未使用資料或新的前瞻量測。
- **操作與輸出**：預覽規則影響、定位第一個／選定修正、標記排除區間及原因、保存不可變 clean dataset 與完整 lineage。預覽抽樣標記清楚，輸出及辨識使用完整資料。
- **空態／故障**：長缺口保留斷點並阻止不符合契約的研究；停機／故障資料保留原件與標籤，不為讓曲線漂亮刪掉。不能以清洗隱藏真實控制失敗或把補值當實測。

#### M5 模型辨識與資格｜3D 預設收合

- **內容**：模型族 FIR／FOPDT／SOPDT 與當前執行環境實際支援狀態、工況與輸入座標、取樣／記憶長度／正則化／延遲範圍、MV→CV 與 DV→CV 響應矩陣；gain／tau1／tau2／dead time 的單位與來源。
- **資格圖**：訓練及選模驗證的多步預測／實測、殘差、激勵／共線／條件數、響應尾端與物理合理性、各 CV 誤差及基準。零增益必須有可辨識證據；未激勵不等於無耦合。參數不確定性只在有估計方法與證據時顯示。
- **操作與輸出**：依凍結 train／validation 規則選模型，保存不可變 model artifact、適用工況、資料與識別程式身分。最終 test 不顯示為可反覆挑模型的排行榜；不能在看過 test 後繼續同一輪選模。
- **空態／故障**：方法不支援、積分／非線性限制、資料激勵不足或模型不合格要給具體原因與可行下一步；不能默默退回另一方法或把 twin 的擬合精度說成工廠保真度。

#### M6 MPC 設定與自動調參｜3D 預設收合

- **內容**：凍結控制模型與分開的 evaluation_plant artifact、實作支援的控制方法、Ts／P／M、每個可搜尋欄位及界線、固定評分與硬限制、基準、開發情境集與計算預算。比較各 CV 變異／偏差／違限與 MV 動作，不只有總分；歷史預測誤差不是 MPC tuning 的閉環評分。
- **執行區**：round／trial 進度、父候選與參數差異、評分來源、無效／逾時／不可行候選及原因、資源與取消狀態。顯示完整參數，不只縮放因子。
- **操作與輸出**：開始有界搜尋、取消、檢視 trial、凍結開發情境集勝出候選。M6 的 Ts 與模型記憶唯讀，改動必須返回 M3／M4／M5 建立新分支；不可在單一 trial 內回頭重建上游。P／M 僅在執行器與比較契約支援時才可搜尋，不假設與模型記憶相同，也不截斷已辨識尾端。所有候選評分的物理時間、工程尺度與硬限制一致。
- **不變條件**：搜尋器不能改資料切分、評分、plant、規格、安全限或核准政策來獲勝；基準可勝出，無改善不強迫產生新版本。
- **工程權衡**：保留所有有效／失敗 trial，展示開發情境下的 CV 品質與 MV 動作折衷／非支配候選；工程師可以在最終保留驗證前依預先聲明的目標選擇保守候選，不只接受總分第一名。選擇與理由凍結後才開封保留情境；最終驗證失敗不刪除其他 trial，但不能回頭以同一已曝光保留集挑下一名並仍稱獨立驗證。
- **Plant 證據**：evaluation_plant 獨立版本化，包含來源分類（合成真值／獨立實驗模型／同源觀測假說）、係數及動態、初始化、適用工況、驗證證據與不確定性集合。搜尋期間凍結，不能由候選改動。從同一歷史推導的 SOPDT/FIR 仍是同源假說；增益／延遲／非線性失配範圍依工程或資料證據設定，不硬套固定百分比，也不因加入失配就宣稱獨立工廠真值。
- **致動器座標**：明確區分 MPC 命令 SP、已接受／已應用 SP、製程實際流量／速度及 OP。若模型以命令 SP 辨識且已包含下層 PID／致動器動態，不再重複串接同一慣性；若以實際製程輸入辨識，則模擬須補足有依據的命令至製程輸入動態／限制。應用確認、追隨偏差與動作代價依該契約評估，不能把 OP 當同單位回授。

#### M7 閉環模擬與壓力驗證｜3D 預設展開

- **內容**：plant／控制器／方法／trial／情境／傳輸／種子、初始狀態與初始化假設、時間／掃描數；3D、已確認 frame、CV／MV／DV 三區實際／完整預測／累積 MV 計畫。明示 plant 是獨立真值、歷史辨識假說或不確定性集合。
- **操作與輸出**：建立新 session、單步、連續、批次完成後暫停、關閉、完成後重開與重跑；基準／候選使用配對的初值、DV、噪聲及時間窗。拖動封存游標是回放，不改模擬器狀態；想從中段重啟須有有效狀態快照。
- **情境與邊界**：名義、原料／負荷變化、增益／延遲漂移、致動器限制及支援的通訊故障，各自保存設定與結果；未實作情境不可只靠動畫演出。
- **輸出與失效**：完整軌跡、forecast、品質、命令／應用證據與 source hashes。原驗收的 129,600 掃描保持不變；其他工況的驗證長度按動態及風險預先核准，不能用三個月樣本數直接證明旋窯／煤磨安全。失敗／提前關閉不當完成。快照與 3D 收合動作本身不得發出 advance、pause 或額外掃描；已啟動的連續執行照既定排程進行，已暫停的執行維持暫停。
- **執行擁有者**：新版連續模擬由伺服器 job 擁有有界批次排程，瀏覽器只送建立／單步／連續／暫停／關閉命令與訂閱狀態；不依賴背景分頁計時器。這是相對既有瀏覽器排送方式的必要改動，須以關頁／重連／重啟案例驗證。程序無合法 checkpoint 時標中斷並新建執行，不假稱無縫續算。
- **畫面串流與重連**：後端邏輯掃描及完整封存與顯示速率分離；每個有界 snapshot 帶 session／run、sequence、sample、模型／場景版本及模擬時間。重連先取得原子快照，丟棄過期／跨版本回覆與中間動畫，不補播累積視覺影格；歷史圖以有界視窗另查。顯示最後確認掃描、資料年齡與是否落後；若顯示加速比，使用實測完成掃描／牆鐘區間計算。暫停等待已接受批次結束，重連不改控制時鐘或另建 session。

#### M8 比較、候選與送審｜3D 預設收合

- **內容**：候選與基準的配對比較、各 CV／MV 工程指標、改善與代價、違限、worst-case 情境、適用工況、限制與完整證據清單；醒目區分模擬、影子觀察與現場證據。
- **兩種留出證據分開**：M5 的歷史 train／validation／test 以記錄 MV／DV 預測留出實測 CV，驗證辨識泛化。M6–M8 的開發情境／封存驗收情境則在凍結 evaluation_plant 上產生反事實 CV，驗證模型條件下的閉環穩健性；可以使用歷史留出 DV／目標工況，但不能用錄好的 CV 評分未實施 MV，也不能標成工廠真實改善。
- **選擇與資格**：tuning 排名只使用開發情境集；模型／tuning 凍結後才執行預先登錄、未用於選參的閉環驗收情境與相應歷史預測 test。任一結果被用來換候選／改規則，就失去該留出資格；新一輪需新的未使用保留資料／情境，不能只換亂數種子假裝獨立。現場受控投運的績效為第三類證據，不由模擬替代。
- **操作與輸出**：保留基準／選擇候選、執行最終資格檢查、保存 candidate bundle、送審或撤回尚未核准的提交。bundle 保存 lineage、模型／tuning 的允許載荷、evaluation_plant／情境與程式身分、目標設備 ID 集合及 Runtime DeploymentProfile hash；設備／映射／安全資料只作對照證據，不是可套用的 Runtime 設定。發布器採允許欄位清單，拒絕未知及敏感變更，不以 JSON merge 整包覆寫現場。送審不安裝、不啟動、不改現場。
- **失效**：來源被修改、結果不完整、比較不相容、證據缺失或過期均顯示缺項；不能因 HTTP 成功、研究 completed 或單一綜合分數就給綠色「可投運」。

#### M9 持續研究政策｜3D 預設收合

- **內容**：按控制群組配置資料最小量／品質／工況條件、漂移持續時間、觸發冷卻、允許模型族／調參範圍、計算預算、併發限制、保留期及通知；展示最近觸發／跳過／拒絕原因與其研究 run。
- **操作與輸出**：保存／啟用／暫停版本化研究政策。政策只可觸發 M1–M8 的隔離工作；不允許任意程式碼、shell、模型提供者、網址或現場寫入。啟用持續研究不等於啟用自動發布。
- **失效與恢復**：重啟以來源區間／政策／版本識別去重，不重複啟動相同工作；資料不足／品質失效／磁碟或預算不足時停止新研究並告警，不影響現場控制。資料有問題時先診斷，不每次漂移都自動重訓。
- **授權範圍**：若部署允許有界自動升版，本頁僅唯讀顯示 R6 核准政策的狀態與連結，不能編輯其權限或把觸發研究當作升版命令。

### 共用資料、權限與失效契約

1. **不可變產物圖**：raw／clean／engineering／equipment → control_model；evaluation_plant、開發情境集與封存驗收情境集是分開的不可變輸入；它們與 control_model → tuning trial／simulation → candidate → approval → installed／active。DeploymentProfile 由 Runtime 另行核准並以 hash 參照，不由研究產物覆寫。每項保存父項 ID／雜湊、來源獨立性、工況、時鐘、變數順序／單位、schema 與執行版本；草稿有 revision。上游改動建立新分支，下游舊結果可讀但標不適用，不就地改寫。
2. **精確 context**：請求與快取至少區分 workspace、plant／line／area／group、source、version 及 run／session。切換後取消舊讀取並拒絕遲到回覆，來源缺失不能借用上一頁值。偏好資料不含控制權、秘密或待執行命令。
3. **權限不是模式名稱**：`runtime.view`、`runtime.operate`、`runtime.approve`、`runtime.deploy`、`runtime.admin`、`modeling.edit`、`modeling.run`、`modeling.submit`、`modeling.policy` 按製程範圍由後端判定；它們是擬定的能力契約，不宣稱現有程式已實作。每個寫入路徑都要服務端授權、CSRF／來源保護與審計；研究端無現場憑證，跨來源不使用寬鬆 CORS。
4. **狀態完整**：每頁區分未配置、載入中、空資料、有效、部分資料、過期／失聯、無權限、失敗；只有零筆資料不能等同沒告警／零違限。有限前值可以帶品質顯示，但不可通過寫入 gate；結果未知須保留可查操作 ID。
5. **可追溯執行**：job、import、session、deployment、command 的狀態與冪等操作 ID 由後端持久登錄，而非只放前端記憶體。重新登入／跨來源返回時，按使用者可查看的設備範圍查詢未結操作及結果，不需要帶回舊寫入 token；Runtime 全域列顯示待確認操作與目標。建模只可接收另行授權的唯讀狀態，不取得查詢以外控制能力。登錄在外部副作用前完成；崩潰跨越副作用且無法核對時維持結果未知，不重發。進度依真實完成工作回報，查詢／重開不重啟執行。
6. **不竄改的安全顯示**：模式、target、界限、版本變更為明確事件；統計是否重置與有效比較區間可追溯。OPC DA／UA 品質碼分開解析；量測時間、接收時間、化驗取樣／報告時間與預測 valid time 不混用。
7. **性能預算與資料完整**：查詢有界、長表格虛擬化、圖表展示抽樣保留極值／缺口／品質；辨識、評分及封存使用完整資料。3D 不阻塞數值與操作；WebGL 失敗可降級設備表，但仍算 3D 驗收未通過。實際容量與 3D 性能按指定硬體、設備與點位數量量測。
8. **安全不藏在說明文字**：操作被禁止時說明原因及解除條件；主要區只顯示需要的結論、數值及下一步，工程細節按需展開。重要限制與來源永久可見，不能靠使用者閱讀長文避免誤動作。

### 審查與驗收清單

- 功能：16 個頁面皆有自己的主要內容、可用操作、前置條件、輸出及空態；不是同一個 3D 看板替換標題。四製程均可導航，未配置者明確顯示未配置。
- 呈現：3D 展開／收合／重新定位不改來源、草稿、選線、游標或 job；呈現操作本身不發出模擬 advance／pause、不增加或刪除掃描，既有排程照常；跨版本不還原錯誤相機／設備。手機與鍵盤可完成相同業務流程。
- 資料：不混用時間基準、來源或變數單位；原始、補值、預測、命令、回授及 OP 可區分；同一試驗相容性與完整期績效可獨立重算。
- 研究：修改 test 不影響候選選擇；修改規格／評分必須新研究；未激勵、因果性不足、失敗 trial、來源改動與不適用模型均不能得到投運資格。
- 控制：跨製程請求、權限撤銷、過期表單、多分頁競爭、重複操作、部分寫入、逾時後晚到回授與斷線均走正式路徑驗證。重連、切頁、解除告警不自動重新接管。
- 交付：設計審查結果與產品運行證據分開保存。設計原型不執行資料清洗、MPC、OPC 或權限隔離；不能用 prototype 的操作成功代替正式產品驗收。

### 設計審查處置

本輪由兩個獨立唯讀 reviewer 分別檢查 Runtime 操作／授權與建模／統計流程；這不是外部模型廠商背書，也不構成安全認證。採納並補強：後端在途操作登錄、Engage MV 互斥、設備實例與 Runtime profile 綁定、受控退出、委任發布與重新接管區分、兩類留出證據、evaluation_plant 產物、慢化驗／估測契約，以及模擬排程與呈現分離。

不採納無現場依據的固定增益／延遲範圍、對所有旋窯／煤磨一概宣稱不能使用線性控制，或預設 ABB Auto／Cascade 模式切換命令。四製程工作區範圍不縮減；各控制群組按實際模型能力、工況、量測及安全證據准入，不把通用 UI 當成所有製程已能投運。

持續研究另有可辨識性准入：區分僅更新評估情境／調參與重新辨識動態。新資料多、條件數良好或殘差變大本身不能證明閉環辨識無偏；動態重辨識須檢查激勵、外生變化來源與方法對閉環混淆的適用證據。證據不足時保留已核准模型，提出收集資料或經審查的辨識試驗，不自動向現場注入激勵。可進行明示 exploratory 分析，但不得進入自動發布資格。

### 本輪設計原型驗證

- 以瀏覽器操作 `hmi/frontend/design/expert-twin-workspace.html`，檢查 16 頁 × 四製程 × 兩個 viewport（1440×1000、390×844），共 128 組：頁面標題／製程場景一致、設備選擇入口可見、3D 區預設／切換／恢復預設正確、無整頁水平溢位。
- 實際操作鍵盤 Enter 收合／展開、跨頁草稿保留、製程與工作區選取隔離、趨勢勾選保留、桌面／手機獨立收合偏好、三種資料來源表單切換。視窗改變以 media-query 更新後的 DOM 狀態確認，不把同步讀取時序當成產品錯誤。
- 檢查 Runtime 總覽、建模清洗及手機模擬頁截圖；手機捲動到底部時，固定來源／版本／控制狀態區仍可見（頂端 0、高度 155，viewport 高度 844）。頁面錯誤記錄為空。
- 兩路獨立設計審查的後續修正包含動態狀態初始化、DCS 未確認交接互鎖、共享 MV 固定責任、模擬重連、閉環辨識准入、固定 Ts／模型記憶與多目標折衷。未採納以操作員按鈕替代現場交接證據、固定等待 N 步當成無擾動保證，或重用已曝光最終驗證集選下一候選。
- **驗證範圍**：這是逐頁設計原型，不是已整合的 React 工作區；設備仍為靜態草圖，無真實控制、資料訓練、WebGL 或 ABB 通訊。此結果不取代產品三個月閉環、後端授權、真 3D 效能或現場試車驗收，也不是外部模型廠商認證。
