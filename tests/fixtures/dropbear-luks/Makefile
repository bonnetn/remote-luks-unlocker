.PHONY: test-server test-connection stop-test-server clean-test-data

test-server:
	./scripts/start-test-server.sh

test-connection:
	./scripts/test-connection.sh

stop-test-server:
	./scripts/stop-test-server.sh

clean-test-data:
	rm -rf "$${DROPBEAR_DATA_DIR:-.test-data}"
