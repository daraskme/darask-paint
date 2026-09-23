# NixOS / Linux

## 本体

NixOS のデスクトップセッションで、リポジトリを clone して実行します。

```sh
nix run .
nix run . -- /absolute/path/image.png
```

Nix の実験的機能 `nix-command` と `flakes` が必要です。無効な場合は NixOS 設定に
`nix.settings.experimental-features = [ "nix-command" "flakes" ];` を指定してください。
システムの変更は自動では行いません。グラフィックスドライバーは通常の NixOS デスクトップ設定を使用します。

`nix build` の結果は `result/bin/darask-paint`、ユーザープロファイルへの導入は `nix profile add .` です。
flake.lock に Rust / ネイティブ依存の nixpkgs リビジョンを固定しています。
本体は x86_64-linux / aarch64-linux の出力を持ち、実機検証は x86_64-linux です。

Wayland と X11 を有効にしています。ファイルダイアログはデスクトップの XDG portal を優先し、
利用できない場合は同梱の Zenity にフォールバックします。NixOS の KDE / GNOME では通常の
portal 設定が使えます。その他の環境では `xdg.portal.enable` とデスクトップに合う backend を設定してください。
画像クリップボードには X11 と Wayland data-control のサポートを有効にしています。
Wayland 側はコンポジターの data-control 対応に依存します。

日本語表示とテキストツールには IPAex ゴシックを Nix 依存として供給します。
`DARASK_FONT_FILE=/absolute/path/font.ttf nix run .` で変更できます。
Nix 以外からビルドした Linux バイナリは Fontconfig (`fc-match`) で日本語フォントを探索します。
Windows は従来の游ゴシック・メイリオ等を使用します。

## 書き込み先

| 内容 | Linux の既定値 |
| --- | --- |
| 設定 | `$XDG_CONFIG_HOME/darask-paint/settings.txt` (未設定時 `~/.config/darask-paint/settings.txt`) |
| プラグイン ZIP / 展開先 | `$XDG_DATA_HOME/darask-paint/plugins` (未設定時 `~/.local/share/darask-paint/plugins`) |
| IOpaint 環境 | `$XDG_DATA_HOME/darask-paint-iopaint` |
| Diffusion 環境 / モデル | `$XDG_DATA_HOME/darask-paint-ai-diffusion` |

空または相対パスの XDG 変数は既定値に戻します。実行ファイルが `/nix/store` にあっても、
設定・環境・モデルは書き込み可能なユーザーディレクトリへ保存します。
プラグインフォルダは本体の設定画面で変更できます。Windows の既定値は変更していません。

## AI プラグイン

本体と同様に NixOS 対応版の `darask-paint-iopaint` / `darask-paint-ai-diffusion` を使用します。
各リポジトリで `nix run .` を実行してから本体の AI メニューを使えます。
または各配布 ZIP を本体のプラグインフォルダに置くと、初回の AI 操作時に展開・起動します。
古い Windows 専用 ZIP には Linux ランチャーがないため、対応版へ更新してください。

Linux では manifest の `launcherLinux` (`darask-plugin.sh`) を Bash で起動します。
ZIP 作成時に実行権限が失われても動作します。ZIP の展開は GNU tar ではなく libarchive の
`bsdtar` を使います。これらと進捗表示用 `xterm` は本体の Nix パッケージに含まれます。
xterm は Wayland セッションでは XWayland を使用します。XWayland を無効にしている環境では、
プラグインを別の端末から `nix run .` で起動してください。
端末を閉じるか Ctrl+C でプラグインを停止できます。

初回はネットワーク接続が必要で、Python パッケージとモデルをダウンロードします。
PyTorch は CPU 版が既定です。Diffusion の既定モデルは約 2 GB で、端末で確認してから取得します。
NVIDIA CUDA 12.8 対応ドライバーがある場合は、プラグイン側で
`DARASK_TORCH_BACKEND=cu128 nix run .` を使用できます。CUDA の動作は実機環境ごとに確認してください。
AMD / Intel GPU の自動セットアップは提供せず、CPU で動作します。

IOpaint は v2.0.0-rc2 のコミットに固定し、`--darask-plugin-mode` を必須にしています。
HTTP は従来どおり 127.0.0.1 の 8423 / 8424 ポートです。
初回準備が本体の待機時間を超えた場合は、端末のセットアップ完了を待って AI 操作を再実行します。

## 開発・確認

```sh
nix develop -c cargo fmt --check
nix develop -c cargo clippy --all-targets -- -D warnings
nix develop -c cargo test
nix flake check
DARASK_BENCH=1 nix run .
```

最後のコマンドは GUI の最初の描画後に終了し、作業ディレクトリに `bench.txt` を保存します。
Windows での動作確認は Windows VM 内で実施してください。
