PREFIX ?= /usr/local
BINDIR = $(PREFIX)/bin
SYSTEMD_DIR = /etc/systemd/system
# `install` needs root to write /usr/local/bin and /etc/systemd/system, so it is
# normally run under sudo -- where `whoami` is root and $(HOME) is /root. Baking
# those into the unit makes the daemon run as root and look for config in
# /root/.config/rookery, which does not exist; it then crash-loops until systemd
# gives up with "start request repeated too quickly".
#
# SUDO_USER is the invoking user, so prefer it. Falling back to `whoami` keeps a
# non-sudo install (a custom PREFIX in $HOME) working.
SERVICE_USER ?= $(or $(SUDO_USER),$(shell whoami))
# Likewise resolve HF_HOME against the invoking user's home, not root's -- a
# container backend bind-mounts this, so /root/.cache/huggingface would also
# hide every model on the box.
SERVICE_HOME ?= $(shell getent passwd $(SERVICE_USER) | cut -d: -f6)
HF_HOME ?= $(if $(SERVICE_HOME),$(SERVICE_HOME),$(HOME))/.cache/huggingface

# Only the install steps need root. Building under sudo breaks three ways at once:
# trunk lives in the invoking user's ~/.cargo/bin and is not on root's PATH, HOME
# becomes /root so cargo uses a different (empty) registry and re-downloads the
# world, and every artifact left in target/ ends up root-owned, so the next
# ordinary `cargo build` fails on permissions. Drop back to the invoking user for
# anything that compiles. Empty when not under sudo, where the commands run as-is.
# An explicit minimal PATH rather than $(PATH): inheriting the caller's would drag
# in whatever happens to be in their shell, which is neither reproducible nor
# portable. cargo and trunk both live in ~/.cargo/bin.
RUNAS = $(if $(SUDO_USER),sudo -u $(SERVICE_USER) env HOME=$(SERVICE_HOME) PATH=$(SERVICE_HOME)/.cargo/bin:/usr/local/bin:/usr/bin:/bin,)

.PHONY: build install uninstall enable disable restart dashboard clean test chaos-test

# NOTE: `build` does NOT rebuild the dashboard. rookeryd embeds
# crates/rookery-dashboard/dist via include_dir!, and that crate is excluded from
# the workspace, so a plain cargo build silently ships whatever dist/ is committed.
# Use `make dashboard` after changing dashboard source, or `make install` which
# does it for you.
build:
	$(RUNAS) cargo build --release

# --remap-path-prefix keeps absolute build paths out of the artifact. dist/ is
# committed (rookeryd embeds it via include_dir!) and rustc bakes source paths
# into dependency panic messages, so without this the checked-in bundle carries
# the build machine's directory layout.
dashboard:
	cd crates/rookery-dashboard && \
	$(RUNAS) RUSTFLAGS="--remap-path-prefix=$(SERVICE_HOME)/.cargo/registry=/cargo/registry --remap-path-prefix=$(SERVICE_HOME)/.rustup=/rustup --remap-path-prefix=$(CURDIR)=/build $$RUSTFLAGS" \
	trunk build --release
	@# Trigger re-embed into daemon binary
	touch crates/rookery-daemon/src/routes.rs
	$(RUNAS) cargo build --release -p rookery-daemon

install: dashboard build
	install -d $(DESTDIR)$(BINDIR)
	install -d $(DESTDIR)$(SYSTEMD_DIR)
	@# Install via temp + rename: mv(1) within a filesystem is an atomic rename(2),
	@# so the path is never absent or partial and a running daemon keeps its old
	@# inode. Writing over the live binary in place would fail with ETXTBSY.
	install -m 755 target/release/rookeryd $(DESTDIR)$(BINDIR)/rookeryd.new
	mv -f $(DESTDIR)$(BINDIR)/rookeryd.new $(DESTDIR)$(BINDIR)/rookeryd
	install -m 755 target/release/rookery $(DESTDIR)$(BINDIR)/rookery.new
	mv -f $(DESTDIR)$(BINDIR)/rookery.new $(DESTDIR)$(BINDIR)/rookery
	@echo "Installed rookeryd and rookery to $(BINDIR)"
	@# Generate systemd unit from template
	@sed \
		-e 's|@BINDIR@|$(BINDIR)|g' \
		-e 's|@USER@|$(SERVICE_USER)|g' \
		-e 's|@HF_HOME@|$(HF_HOME)|g' \
		rookery.service.in > rookery.service.generated
	install -m 644 rookery.service.generated $(DESTDIR)$(SYSTEMD_DIR)/rookery.service
	@rm -f rookery.service.generated
	@echo "Installed rookery.service to $(SYSTEMD_DIR)"
	@echo "  User=$(SERVICE_USER)"
	@echo "  HF_HOME=$(HF_HOME)"
	@echo ""
	@echo "  ^ check both. The daemon reads config from ~USER/.config/rookery and"
	@echo "    bind-mounts HF_HOME for container backends; wrong values crash-loop"
	@echo "    or hide your models. Override with:"
	@echo "      sudo make install SERVICE_USER=you HF_HOME=/path/to/models"
	@echo ""
	@echo "Next steps:"
	@echo "  sudo systemctl daemon-reload   # REQUIRED: the unit changed on disk"
	@echo "  sudo systemctl restart rookery"

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/rookeryd
	rm -f $(DESTDIR)$(BINDIR)/rookery
	rm -f $(DESTDIR)$(SYSTEMD_DIR)/rookery.service
	@echo "Uninstalled rookery"

enable:
	systemctl daemon-reload
	systemctl enable --now rookery

disable:
	systemctl disable --now rookery

restart:
	systemctl restart rookery

test:
	cargo test --workspace

chaos-test:
	./tests/chaos/run-all.sh

clean:
	cargo clean
