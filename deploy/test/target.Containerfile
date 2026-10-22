# Throwaway deployment target for test/dry-run.sh: a plain Debian trixie with systemd and SSH, as
# a fresh VPS would be. The playbook installs everything else (podman, ...).
FROM docker.io/library/debian:trixie

RUN apt-get update \
  && apt-get install -y --no-install-recommends systemd systemd-sysv dbus openssh-server sudo python3 \
     ca-certificates socat curl iproute2 \
  && rm -rf /var/lib/apt/lists/* \
  && useradd -m -s /bin/bash deploy \
  && echo 'deploy ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/deploy \
  && mkdir -p /home/deploy/.ssh && chmod 700 /home/deploy/.ssh \
  && systemctl enable ssh \
  # Nothing to do in a container: no console, no kernel modules to load.
  && systemctl mask getty@tty1.service console-getty.service systemd-modules-load.service

# Docker configures the network before systemd starts, so nothing would ever reach
# network-online.target, which Quadlet's user units wait for. A VPS has a real provider
# (ifupdown, systemd-networkd-wait-online).
COPY network-online.service /etc/systemd/system/dry-run-network-online.service
RUN systemctl enable dry-run-network-online.service \
  && systemctl add-wants multi-user.target network-online.target

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
