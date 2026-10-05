SHELL := /bin/bash
CAIRN_REPO ?= https://github.com/oritera/Cairn.git
HERMES_PLUGINS ?= $(HOME)/.hermes/plugins
ENGAGEMENT ?= $(HOME)/engagements

.PHONY: help bootstrap up down logs ps plugin stop-all unstop-all smoke clean

help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-14s %s\n", $$1, $$2}'

bootstrap: ## Install/verify the stack (delegates to install.sh)
	@./install.sh

up: ## Start the Cairn server + dispatcher
	docker compose up -d --build
	@echo "Cairn API: http://127.0.0.1:8000/projects"

down: ## Stop the stack (data kept in ./datas/cairn)
	docker compose down

logs: ## Tail dispatcher logs
	docker compose logs -f cairn-dispatcher

ps: ## Show project status
	@curl -s http://127.0.0.1:8000/projects | python3 -m json.tool

plugin: ## Symlink the triad plugin into the active Hermes profile
	@mkdir -p $(HERMES_PLUGINS)
	@ln -sfn $(PWD)/hermes/plugin-triad $(HERMES_PLUGINS)/triad
	@hermes plugins doctor $(PWD)/hermes/plugin-triad || true

stop-all: ## KILL SWITCH — hard-stop every Cairn project
	@for id in $$(curl -s http://127.0.0.1:8000/projects | python3 -c 'import json,sys;print(" ".join(p["id"] for p in json.load(sys.stdin)))'); do \
	  echo -n "stopping $$id ... "; \
	  curl -s -X PUT http://127.0.0.1:8000/projects/$$id/status \
	       -H 'Content-Type: application/json' -d '{"status":"stopped"}' >/dev/null && echo ok; \
	done

unstop-all: ## Resume every stopped project
	@for id in $$(curl -s http://127.0.0.1:8000/projects | python3 -c 'import json,sys;print(" ".join(p["id"] for p in json.load(sys.stdin)))'); do \
	  curl -s -X PUT http://127.0.0.1:8000/projects/$$id/status \
	       -H 'Content-Type: application/json' -d '{"status":"active"}' >/dev/null && echo "resumed $$id"; \
	done

clean: ## Remove all Cairn data (DESTRUCTIVE)
	rm -rf datas/cairn
