#!/bin/bash

insmod dtbocfg.ko
mkdir -p /sys/kernel/config/device-tree/overlays/portc_pwm
cp portc_pwm.dtbo /sys/kernel/config/device-tree/overlays/portc_pwm/dtbo
echo 1 > /sys/kernel/config/device-tree/overlays/portc_pwm/status