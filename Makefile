.PHONY: test-server test-connection stop-test-server clean-test-data

test-server:
	$(MAKE) -C tests/fixtures/dropbear-luks test-server

test-connection:
	$(MAKE) -C tests/fixtures/dropbear-luks test-connection

stop-test-server:
	$(MAKE) -C tests/fixtures/dropbear-luks stop-test-server

clean-test-data:
	$(MAKE) -C tests/fixtures/dropbear-luks clean-test-data
