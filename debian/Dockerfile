FROM debian_armhf_bookworm:latest

ADD CoreMP135_buildroot-external-st/board/m5stack/overlay/boot /boot/
ADD CoreMP135_buildroot-external-st/board/m5stack/overlay/etc /etc/
ADD CoreMP135_buildroot-external-st/board/m5stack/overlay/lib /lib/
ADD CoreMP135_buildroot-external-st/board/m5stack/overlay/usr /usr/
ADD CoreMP135_buildroot-external-st/board/m5stack/overlay/var /var/
ADD CoreMP135_buildroot/output/target/boot        /boot/
ADD CoreMP135_buildroot/output/target/lib/modules /lib/modules
ADD dtbocfg/dtbocfg.ko /lib/modules/5.15.118/kernel/drivers/
RUN depmod -a 5.15.118
RUN echo root:root | chpasswd
RUN cat /etc/apt/sources.list
RUN echo -n "deb http://deb.debian.org/debian bookworm main non-free-firmware" > /etc/apt/sources.list
RUN apt-get update && apt-get install -y \
    usbutils \
    firmware-realtek \
    network-manager \
    fbset \
    openssh-server \
    npm \
    git \
 && apt-get clean \
 && rm -rf /var/lib/apt/lists/*
RUN echo "PermitRootLogin yes" >> /etc/ssh/sshd_config