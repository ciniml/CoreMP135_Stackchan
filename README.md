# Stackchan base firmware for CoreMP135

## 概要

CoreMP135で ｽﾀｯｸﾁｬﾝ を作るためのいろいろを置く予定。今のところはDevice Tree Overlayを使ってPort.CからPWMを出せるようにするものだけ。

## 使い方

CoreMP135向けのDebianイメージを書き込んで起動しておきます。CoreMP135をインターネット接続可能な状態にします。
リポジトリのリリースページからビルド済みのパッケージをダウンロードします。

```
# curlいれていないなら入れる
apt install curl
# パッケージをDL
curl -OL https://github.com/ciniml/CoreMP135_Stackchan/...
# 展開
tar xf portc_pwm.tar.gz
cd portc_pwm
```

Port.CをPWM出力に変更します。

```
./configure_portc.sh
```

Port.CのPWMを初期化して2[ms] と 3[ms] のパルスを出力します。

```
./setup_portc_pwm.sh
```

sysfs経由でのPWMの使い方はSTMicroelectronicsのドキュメントを参照してください。

https://wiki.st.com/stm32mpu/wiki/PWM_overview


## ビルド

CoreMP135のドキュメントにしたがってbuildrootでカーネルをビルドしておきます。
https://docs.m5stack.com/en/guide/linux/coremp135/buildroot

CoreMP135_buildroot と同じディレクトリに本リポジトリをcloneします。

```shell
#git clone https://github.com/m5stack/CoreMP135_buildroot.git
#git clone https://github.com/m5stack/CoreMP135_buildroot-external-st.git
git clone CoreMP135_Stackchan --recursive
```

必要なパッケージを入れておきます。

```shell
sudo apt install gcc-arm-linux-gnueabihf device-tree-compiler
```

dtbocfgやデバイスツリーをビルドしてまとめます。

```
make portc_pwm.tar.gz
```


