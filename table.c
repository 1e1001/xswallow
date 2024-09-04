#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// Mostly based on the Crafting Interpreters hash table chapter

typedef struct {
	uint32_t key;
	void* value;
} Entry;

typedef struct {
	size_t count;
	size_t capacity;
	Entry* entries;
} Table;

void table_init(Table* table) {
	table->count = 0;
	table->capacity = 0;
	table->entries = NULL;
}

// http://burtleburtle.net/bob/hash/integer.html
// I could just use the integer directly but window ids are very structured integers
uint32_t table_hash(uint32_t a) {
	a -= a << 6;
	a ^= a >> 17;
	a -= a << 9;
	a ^= a << 4;
	a -= a << 3;
	a ^= a << 10;
	a ^= a >> 15;
	return a;
}

Entry* table_probe(Entry* entries, size_t capacity, uint32_t key) {
	Entry* tomb = NULL;
	size_t i = table_hash(key) % capacity;
	for (;;) {
		Entry* entry = entries + i;
		if (!entry->value) {
			if (entry->key) {
				if (!tomb)
					tomb = entry;
			} else {
				return tomb ? tomb : entry;
			}
		} else if (entry->key == key) {
			return entry;
		}
		++i;
		i %= capacity;
	}
}

void table_resize(Table* table, size_t capacity) {
	Entry* entries = malloc(capacity * sizeof(Entry));
	memset(entries, 0, capacity * sizeof(Entry));
	table->count = 0;
	for (size_t i = 0; i < table->capacity; ++i) {
		Entry* entry = table->entries + i;
		if (!entry->value)
			continue;
		*table_probe(entries, capacity, entry->key) = *entry;
		++table->count;
	}
	free(table->entries);
	table->entries = entries;
	table->capacity = capacity;
}

void table_add(Table* table, uint32_t key, void* value) {
	if ((table->count + 1) * 4 > (table->capacity) * 3) {
		size_t capacity = table->capacity * 2;
		if (capacity < 8)
			capacity = 8;
		table_resize(table, capacity);
	}
	Entry* entry = table_probe(table->entries, table->capacity, key);
	if (!entry->value && !entry->key)
		++table->count;
	entry->key = key;
	entry->value = value;
}

void* table_get(Table* table, uint32_t key) {
	if (!table->count)
		return NULL;
	return table_probe(table->entries, table->capacity, key)->value;
}

void* table_del(Table* table, uint32_t key) {
	if (!table->count)
		return NULL;
	Entry* entry = table_probe(table->entries, table->capacity, key);
	if (!entry->value)
		return NULL;
	void* ret = entry->value;
	entry->value = NULL;
	entry->key = 1;
	return ret;
}
