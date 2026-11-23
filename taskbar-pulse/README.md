# Taskbar Pulse

Windows のタスクバー上に、PC の使用率・温度・バッテリー、再生中のメディア、オーディオビジュアライザーを表示する常駐アプリです。Rust 製で、インストール不要の exe 1 つで動きます。

## できること

| 項目 | 取得方法 | 管理者権限 |
|---|---|---|
| CPU 使用率（全体 / P-CORE / E-CORE） | PDH `Processor Information\% Processor Utility` と `GetSystemCpuSetInformation` の EfficiencyClass | 不要 |
| GPU 使用率 / VRAM 使用量 | PDH `GPU Engine` / `GPU Adapter Memory` | 不要 |
| メモリ / ディスク / ネットワーク | `GlobalMemoryStatusEx`、PDH `PhysicalDisk`、`Network Interface` | 不要 |
| CPU / GPU 温度 | LibreHardwareMonitor の WMI（起動中のみ）→ NVIDIA は NVML → CPU は ACPI Thermal Zone | 不要（LHM 自体は管理者で起動） |
| PC のバッテリー | `GetSystemPowerStatus` | 不要 |
| マウス・コントローラー等のバッテリー | Bluetooth デバイスのプロパティ、Xbox コントローラーは XInput | 不要 |
| 再生中のメディア | Windows のメディアコントロール（SMTC） | 不要 |
| オーディオビジュアライザー | WASAPI ループバック録音 + FFT | 不要 |

- P-CORE / E-CORE は Intel 第12世代以降などのハイブリッド CPU のみ表示されます。
- 表示部分を **左ドラッグ** で位置調整、メディア部分を **クリック** で再生/一時停止、**ホイール** で次/前の曲。
- 表示部分かトレイアイコンを **右クリック** でメニュー（表示項目・動作モード・位置・文字サイズ・自動起動・終了）。

## 見た目のカスタマイズ

右クリックメニューの「カラー」と「見た目・エフェクト」から変更できます。

- **カラーテーマ**: オーロラ / サンセット / ネオン / オーシャン / サクラ / モノクロ / カスタム
- **色を自由に選ぶ**: 「カラー」→ 各色の項目で Windows の色選択ダイアログが開きます（数値・ラベル・グラデーション3色・背景パネル）。選ぶと自動でカスタムテーマに切り替わります
- **ビジュアライザーの形**: バー / ミラー（上下対称）/ 波形ライン / ドット。色はテーマのグラデーション
- **ピークホールド**: 各バーの最大値に小さな印が残り、ゆっくり落ちます
- **ミニ履歴グラフ**: 各数値の後ろに約60秒分の推移を薄く表示
- **値に応じて色を変える**: 使用率・温度が上がるほど 緑 → 黄 → 赤
- **数値をなめらかに変化**: 数値が新しい値へ滑らかに移ります
- **アルバムアート・曲名スクロール**: 再生中の曲のジャケット（YouTube ならサムネイル）を表示し、長い曲名は流れるように表示
- **背景パネル（すりガラス風）**: 半透明の角丸パネルと光沢・縁取り。濃さは3段階。※ 本物の背景ぼかしは、このアプリの透明ウィンドウ方式では Windows の仕様上使えないため、半透明と光沢で表現しています

ピークホールド・なめらかな数値・曲名スクロールなど動きのある効果は、標準 / 高負荷モードでのみ動きます（低負荷モードでは負荷を抑えるため静止表示）。

## 動作モード

| モード | センサー更新 | 描画 | ビジュアライザー |
|---|---|---|---|
| 超低負荷 | 5 秒 | CPU（Direct2D ソフトウェア） | なし |
| 低負荷 | 2 秒 | CPU（Direct2D ソフトウェア） | なし |
| 標準 | 1 秒 | GPU（Direct2D ハードウェア） | 30 fps |
| 高負荷 | 0.5 秒 | GPU（Direct2D ハードウェア） | 60 fps |

無音の間はビジュアライザーの再描画を止めて負荷を抑えます。

### ビジュアライザー感度

右クリックメニュー「ビジュアライザー感度」で切り替えます。

| 感度 | 動き |
|---|---|
| 自動（初期値） | 直近で一番大きい音を基準に自動で増幅。小さい音量でも大きい音量でもバーが動きます |
| 高感度 | 固定で約25dB持ち上げます。大きい音では振り切れやすくなります |
| 標準 | 固定スケール（-60〜0 dBFS） |

## 温度について（重要）

CPU 温度を正確に読むにはカーネルドライバーが必要です。昔よく使われた WinRing0 ドライバーは 2025 年から Microsoft Defender に脆弱なドライバーとして検出されるため、このアプリはドライバーを同梱していません。

- 正確な CPU 温度が欲しい場合は [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor) を起動しておくと、その値を自動で読みます。
- 何もない場合は ACPI Thermal Zone の値を出しますが、デスクトップ PC ではマザーボードの値で CPU の実温度と違うことがあります。

## ビルド

Windows 上で:

```powershell
cd taskbar-pulse
cargo build --release
# target\release\taskbar-pulse.exe
```

Linux からのクロスビルド（zig を利用）:

```bash
pip install ziglang && cargo install cargo-zigbuild
rustup target add x86_64-pc-windows-gnu
cargo zigbuild --release --target x86_64-pc-windows-gnu
```

GitHub Actions（`.github/workflows/taskbar-pulse.yml`）でも Windows 用 exe をビルドして成果物として保存します。

## 設定ファイル

`%APPDATA%\TaskbarPulse\config.json`。メニューの「設定ファイルを開く」から編集できます。色（`text_color` など、0xRRGGBB の数値）、ビジュアライザーの本数、メディア名の最大文字数などはここで変えられます。

## 既知の制限

- Windows 11 ではタスクバーへの埋め込み（デスクバンド）が廃止されたため、タスクバーの上に透明なウィンドウを重ねる方式です。
- タスクバーの自動非表示、縦置きタスクバー、サブモニターのタスクバーには未対応です。
- Bluetooth バッテリーのプロパティキーは Microsoft の公開ヘッダーにない非公開の値で、デバイスによっては取得できません。
