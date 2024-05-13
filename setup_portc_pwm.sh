#!/bin/bash

echo 0 > /sys/class/pwm/pwmchip0/export
echo 1 > /sys/class/pwm/pwmchip0/export

# Configure PWM period to 20[ms] 
echo 20000000 > /sys/class/pwm/pwmchip0/pwm0/period
echo 20000000 > /sys/class/pwm/pwmchip0/pwm1/period

# Configure PWM duty ()
echo 3000000 > /sys/class/pwm/pwmchip0/pwm0/duty_cycle
echo 2000000 > /sys/class/pwm/pwmchip0/pwm1/duty_cycle

# Enable PWM
echo 1 > /sys/class/pwm/pwmchip0/pwm0/enable
echo 1 > /sys/class/pwm/pwmchip0/pwm1/enable
