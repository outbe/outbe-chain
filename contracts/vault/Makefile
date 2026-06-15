# Include environment-specific .env file.
# Override ENV_FILE to use a different environment (e.g., make deploy-local ENV_FILE=.bsc-testnet.env)
#ENV_FILE ?= .anvil.env
#-include $(ENV_FILE)
#export

.PHONY: all
all: help

## Solidity
.PHONY: build
build: ## Build Solidity contracts using forge
	forge build

.PHONY: install
install: ## Restore soldeer dependencies
	forge soldeer install

.PHONY: export-abi
export-abi: build ## Export contracts ABI
	@echo 'Exporting ABI for smart contracts'
	mkdir -p ./abi-export
	rm -rf ./abi-export/*.json
	for contract in IVaultProvider \
 		IVaultV2 \
 		; \
	do \
		file=$$(ls -1 ./out/$${contract}.sol/$${contract}.json ./out/$${contract}.sol/$${contract}.*.json 2>/dev/null | tail -1); \
		jq -r '.abi' "$$file" > ./abi-export/$${contract}.json; \
	done

.PHONY: test
test: ## Test Solidity contracts using forge
	forge test

.PHONY: fmt
fmt:  ## Format code
	forge fmt

.PHONY: lint
lint: ## Lint Solidity code
	forge lint

.PHONY: clean
clean: ## Clean up forge artifacts
	forge clean

.PHONY: clean-all
clean-all: clean  ## Clean all build artifacts, and caches
	forge soldeer clean
	rm -rf ./out
	rm -rf ./build
	rm -rf ./cache

GREEN  := $(shell tput -Txterm setaf 2)
YELLOW := $(shell tput -Txterm setaf 3)
WHITE  := $(shell tput -Txterm setaf 7)
CYAN   := $(shell tput -Txterm setaf 6)
RESET  := $(shell tput -Txterm sgr0)

## Help
.PHONY: help
help: ## Show this help
	@echo ''
	@echo 'Usage:'
	@echo '  ${YELLOW}make${RESET} ${GREEN}<target>${RESET}'
	@echo ''
	@echo 'Targets:'
	@awk 'BEGIN {FS = ":.*?## "} { \
		if (/^[a-zA-Z_-]+:.*?##.*$$/) {printf "    ${YELLOW}%-30s${GREEN}%s${RESET}\n", $$1, $$2} \
		else if (/^## .*$$/) {printf "  ${CYAN}%s${RESET}\n", substr($$1,4)} \
		}' $(MAKEFILE_LIST)
