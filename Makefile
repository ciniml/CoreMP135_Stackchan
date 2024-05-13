LINUX_KERNEL_SRC ?= $(abspath ../CoreMP135_buildroot/output/build/linux-custom)

.PHONY: all
all: portc_pwm.dtbo dtbocfg/dtbocfg.ko

.PHONY: clean
clean:
	-$(RM) *.dtbo
	cd dtbocfg; $(MAKE) clean

%.dtbo: %.dts
	-# https://qiita.com/ikwzm/items/b07af1a861d6f1c0fde2
	gcc -E -P -x assembler-with-cpp -I $(LINUX_KERNEL_SRC)/arch/arm/boot/dts -I $(LINUX_KERNEL_SRC)/include $< | dtc -I dts -O dtb -i $(LINUX_KERNEL_SRC)/arch/arm/boot/dts -o $@ -@

dtbocfg/dtbocfg.ko:
	cd dtbocfg; $(MAKE) ARCH=arm KERNEL_SRC=$(LINUX_KERNEL_SRC)

portc_pwm.tar.gz: dtbocfg/dtbocfg.ko portc_pwm.dtbo setup_portc_pwm.sh configure_portc.sh
	mkdir -p portc_pwm
	for f in $^; do cp $$f portc_pwm/; done
	tar cf $@ portc_pwm