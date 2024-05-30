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

.PHONY: build-debian
build-debian: dtbocfg/dtbocfg.ko
	# docker build -t coremp135:latest -f debian/Dockerfile .
	# ID=`docker create coremp135:latest /bin/bash` && docker export $$ID -o coremp135.tar && docker rm $$ID
	docker export 49ef060e9ce9 -o coremp135.tar
	$(RM) coremp135_rootfs.img
	truncate -s 2G coremp135_rootfs.img
	mke2fs -t ext4 coremp135_rootfs.img
	mkdir -p mnt
	sudo mount -o loop coremp135_rootfs.img mnt
	sudo tar xf coremp135.tar -C mnt
	sudo umount mnt

.PHONY: init-sd
init-sd:
	sudo dd if=CoreMP135_buildroot/output/images/sdcard.img of=/dev/sdc bs=1M
	sudo parted -f -s /dev/sdc resizepart 5 100%

.PHONY: copy-debian
copy-debian:
	sudo dd if=coremp135_rootfs.img of=/dev/sdc5 bs=1M
	sudo e2fsck -f /dev/sdc5
	sudo resize2fs /dev/sdc5
	