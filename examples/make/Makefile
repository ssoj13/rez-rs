PREFIX ?= /usr/local
DESTDIR ?=

BINDIR = $(PREFIX)/bin

all: bin/example_make

bin/example_make: main.c
	mkdir -p bin
	$(CC) -o bin/example_make main.c

install: bin/example_make
	mkdir -p $(DESTDIR)$(BINDIR)
	cp bin/example_make $(DESTDIR)$(BINDIR)/

clean:
	rm -f bin/example_make

.PHONY: all install clean
