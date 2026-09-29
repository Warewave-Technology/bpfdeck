# An SSH target for the remote feature (docs/design-remote.md): the real-kernel image plus
# sshd and three users. Keys are mounted at run time (tests/realhost/sshd.sh).
FROM bpfdeck-realhost
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openssh-server sudo >/dev/null \
 && mkdir -p /run/sshd \
 && printf 'PermitRootLogin prohibit-password\nPasswordAuthentication no\n' >> /etc/ssh/sshd_config \
 && useradd -m -s /bin/bash ops && echo 'ops ALL=(root) NOPASSWD: ALL' > /etc/sudoers.d/ops \
 && useradd -m -s /bin/bash pw && echo 'pw:secret-pw' | chpasswd && echo 'pw ALL=(root) ALL' > /etc/sudoers.d/pw
COPY start.sh /start.sh
CMD ["/bin/sh", "/start.sh"]
