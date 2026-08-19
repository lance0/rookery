PREFIX ?= /usr/local
BINDIR = $(PREFIX)/bin
SYSTEMD_DIR = /etc/systemd/system
SERVICE_USER ?= $(shell whoami)
HF_HOME ?= $(HOME)/.cache/huggingface

.PHONY: build install uninstall enable disable restart dashboard clean test chaos-test

build:
	cargo build --release

dashboard:
	cd crates/rookery-dashboard && trunk build --release
	@# Trigger re-embed into daemon binary
	touch crates/rookery-daemon/src/routes.rs
	cargo build --release -p rookery-daemon

install: build
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
	@echo ""
	@echo "Next steps:"
	@echo "  sudo systemctl daemon-reload"
	@echo "  sudo systemctl enable --now rookery"

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
