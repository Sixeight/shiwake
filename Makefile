.PHONY: help install uninstall test

help:
	@printf '%s\n' \
		'make install    Install shiwake with cargo install --path .' \
		'make uninstall  Remove the globally installed shiwake binary' \
		'make test       Run the Rust test suite'

install:
	cargo install --path .

uninstall:
	cargo uninstall shiwake

test:
	cargo test
