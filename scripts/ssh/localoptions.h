/* Verso's build options for upstream Dropbear (all options and their defaults
 * are in third_party/dropbear/src/default_options.h). scripts/build-ssh.sh
 * puts this file next to a copy of the sources. The paths are where
 * scripts/deploy.sh installs the server on the Kindle.
 * See docs/device-tools.md#ssh-developer-only. */

/* Key login only, no passwords. */
#define DROPBEAR_SVR_PASSWORD_AUTH 0

/* Not needed for development: less code. */
#define DROPBEAR_SVR_AGENTFWD 0
#define INETD_MODE 0
#define DO_MOTD 0

/* Host keys live on the USB storage, so they survive reboots and the
 * fingerprint stays the same. With -R, dropbear creates a missing one at
 * these paths on the first connection. */
#define ED25519_PRIV_FILENAME "/mnt/us/verso/ssh/dropbear_ed25519_host_key"
#define ECDSA_PRIV_FILENAME "/mnt/us/verso/ssh/dropbear_ecdsa_host_key"
#define RSA_PRIV_FILENAME "/mnt/us/verso/ssh/dropbear_rsa_host_key"

/* OpenSSH's sftp-server, for sftp and scp (which uses SFTP by default). */
#define SFTPSERVER_PATH "/mnt/us/verso/bin/sftp-server"
