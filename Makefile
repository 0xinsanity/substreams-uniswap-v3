ENDPOINT ?= mainnet.eth.streamingfast.io:443
ROOT_DIR := $(shell dirname $(realpath $(firstword $(MAKEFILE_LIST))))
GRAPH_CONFIG ?= ../graph-node-dev/config/graphman.toml
SINK_RANGE := "17486656:"

.PHONY: build
build:
	cargo build --target wasm32-unknown-unknown --release

.PHONY: stream
stream: build
	substreams run -e $(ENDPOINT) substreams.yaml map_extract_data_types -s 17486656 -t +1000

.PHONY: jsonl_out
jsonl_out: build
	substreams run -e $(ENDPOINT) substreams.yaml jsonl_out -s 17486656 -t +1000

.PHONY: sink_lines_to_files
sink_lines_to_files: build
	substreams-sink-files \
	run \
	$(ENDPOINT) \
	"$(ROOT_DIR)/substreams.yaml" \
	jsonl_out \
	"$(ROOT_DIR)/sink-files/out" \
	--encoder="lines" \
	--file-working-dir="$(ROOT_DIR)/sink-files/working" \
	--state-store="$(ROOT_DIR)/sink-files/workdir/state.yaml" \
	$(SINK_RANGE)

.PHONY: graph_out
graph_out: build
	substreams run -e $(ENDPOINT) substreams.yaml graph_out -s 17486656 -t +10000

.PHONY: protogen
protogen:
	substreams protogen ./substreams.yaml --exclude-paths="sf/substreams/entity,sf/substreams/rpc,google"

.PHONE: package
package: build
	substreams pack -o substreams.spkg substreams.yaml

.PHONE: deploy_local
deploy_local: package
	mkdir build 2> /dev/null || true
	graph build --ipfs http://localhost:5001 subgraph.yaml
	graph create uniswap_v3 --node http://127.0.0.1:8020
	graph deploy --node http://127.0.0.1:8020 --ipfs http://127.0.0.1:5001 --version-label v0.0.1 uniswap_v3 subgraph.yaml

.PHONE: undeploy_local
undeploy_local:
	graphman --config "$(GRAPH_CONFIG)" drop --force uniswap_v3

.PHONE: test
test:
	cargo test --target aarch64-apple-darwin