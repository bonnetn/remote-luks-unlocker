.PHONY: setup-podman-vm test-server test-connection stop-test-server clean-test-data

setup-podman-vm:
	./scripts/setup-podman-vm.sh

test-server:
	./scripts/start-test-server.sh

test-connection:
	./scripts/test-connection.sh

stop-test-server:
	./scripts/stop-test-server.sh

clean-test-data:
	rm -rf .test-data
