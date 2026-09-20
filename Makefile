.PHONY: setup-podman-vm test-server test-connection stop-test-server clean-test-data

setup-podman-vm:
	$(MAKE) -C tests/fixtures/dropbear-luks setup-podman-vm

test-server:
	$(MAKE) -C tests/fixtures/dropbear-luks test-server

test-connection:
	$(MAKE) -C tests/fixtures/dropbear-luks test-connection

stop-test-server:
	$(MAKE) -C tests/fixtures/dropbear-luks stop-test-server

clean-test-data:
	$(MAKE) -C tests/fixtures/dropbear-luks clean-test-data
