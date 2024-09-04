#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>
#include <unistd.h>
#include <xcb/xcb.h>
#include <xcb/xcb_errors.h>

// Too lazy for a header file so I'm just going to put things here
#include "table.c"

typedef struct {
	uint32_t children;
	pid_t pid;
	xcb_window_t wid;
} Parent;

typedef struct {
	int16_t x, y;
	uint16_t w, h;
	uint32_t d;
} Geometry;

typedef struct {
	Parent* parent;
	Geometry position;
} Child;


int geometry_get(xcb_window_t wid, Geometry* out);
int geometry_set(xcb_window_t wid, Geometry* in);
int pname_list_init();
int root_list_init();
int root_list_change();

// Globals used throughout
struct {
	xcb_window_t* buffer;
	// xcb uses int for lengths, so I will as well :(
	// If you have more than 2 billion windows open I'm impressed
	int length;
	int capacity;
} window_list;
xcb_generic_error_t* err;
char** immune_pnames;
char** terminal_pnames;
xcb_connection_t* connection;
xcb_window_t root_wid;
xcb_atom_t atom_NET_ACTIVE_WINDOW;
xcb_atom_t atom_NET_CLIENT_LIST;
xcb_atom_t atom_NET_WM_PID;
xcb_atom_t atom_NET_WM_DESKTOP;
Table parent_table;
Table child_table;

// http://metan.ucw.cz/blog/things-i-wanted-to-know-about-libxcb.html
// ^ This one puts the error handler in the event loop, but since I'm calling requests that can fail I want to handle those where they're at.
int handle_error(const char* ctx) {
	xcb_errors_context_t* err_ctx;
	printf("Error in %s: ", ctx);
	if (xcb_errors_context_new(connection, &err_ctx)) {
		printf("Out of memory?\n");
	} else {
		const char* major = xcb_errors_get_name_for_major_code(err_ctx, err->major_code);
		const char* minor = xcb_errors_get_name_for_minor_code(err_ctx, err->major_code, err->minor_code);
		const char* extension;
		const char* error = xcb_errors_get_name_for_error(err_ctx, err->error_code, &extension);
		printf("%s:%s %s:%s, res=%"PRIu32" seq=%"PRIu16"\n", major, minor, error, extension, err->resource_id, err->sequence);
		xcb_errors_context_free(err_ctx);
	}
	return 1;
}
// TODO: I never really catch errors, probably fine to just use exit(3)
#define HANDLE_ERROR(ctx) if (err) return handle_error(ctx)
#define RAISE_ERROR(expr) do { int errno = expr; if(errno) return errno; } while (0)
#define ERRM_ALLOC(name) do { printf("Error: Allocating " #name " failed\n"); return 1; } while (0)
#define ERRM_NWIN do { printf("Error: No parent window found\n"); return 1; } while (0)

// TODO: Argument parsing of some kind
// - Help info?
// - Replacement for env vars
// - Alternative command to manually unhide the parent
int main() {
	printf("xswallow by 1e1001\n");
	int screen_id;
	connection = xcb_connect(NULL, &screen_id);
	// Subscribe to root window events
	const xcb_setup_t* setup = xcb_get_setup(connection);
	xcb_screen_iterator_t iter = xcb_setup_roots_iterator(setup);
	while (iter.index < screen_id)
		xcb_screen_next(&iter);
	root_wid = iter.data->root;
	printf("Root window is 0x%x\n", root_wid);
	const uint32_t values[] = { XCB_EVENT_MASK_PROPERTY_CHANGE };
	xcb_change_window_attributes(connection, root_wid, XCB_CW_EVENT_MASK, values);
	// Since I'm not using xcb_ewmh.h I need to manually get the value of some atoms
	xcb_intern_atom_cookie_t atom_naw_cookie = xcb_intern_atom(connection, 0, 18, "_NET_ACTIVE_WINDOW");
	xcb_intern_atom_cookie_t atom_ncl_cookie = xcb_intern_atom(connection, 0, 16, "_NET_CLIENT_LIST");
	xcb_intern_atom_cookie_t atom_nwd_cookie = xcb_intern_atom(connection, 0, 15, "_NET_WM_DESKTOP");;
	xcb_intern_atom_cookie_t atom_nwp_cookie = xcb_intern_atom(connection, 0, 11, "_NET_WM_PID");
	xcb_flush(connection);

	xcb_intern_atom_reply_t* atom_naw_reply = xcb_intern_atom_reply(connection, atom_naw_cookie, &err);
	HANDLE_ERROR("main/atom_naw");
	atom_NET_ACTIVE_WINDOW = atom_naw_reply->atom;
	free(atom_naw_reply);

	xcb_intern_atom_reply_t* atom_ncl_reply = xcb_intern_atom_reply(connection, atom_ncl_cookie, &err);
	HANDLE_ERROR("main/atom_ncl");
	atom_NET_CLIENT_LIST = atom_ncl_reply->atom;
	free(atom_ncl_reply);

	xcb_intern_atom_reply_t* atom_nwd_reply = xcb_intern_atom_reply(connection, atom_nwd_cookie, &err);
	HANDLE_ERROR("main/atom_nwd");
	atom_NET_WM_DESKTOP = atom_nwd_reply->atom;
	free(atom_nwd_reply);

	xcb_intern_atom_reply_t* atom_nwp_reply = xcb_intern_atom_reply(connection, atom_nwp_cookie, &err);
	HANDLE_ERROR("main/atom_nwp");
	atom_NET_WM_PID = atom_nwp_reply->atom;
	free(atom_nwp_reply);

	RAISE_ERROR(pname_list_init());
	RAISE_ERROR(root_list_init());
	table_init(&parent_table);
	table_init(&child_table);

	xcb_generic_event_t* event;
	while ((event = xcb_wait_for_event(connection))) {
		switch (event->response_type & ~0x80) {
		case 0: {
			// Errors from the event loop don't distrupt important calculations, so it's fine ignoring them (at the expense of maybe leaving a useless program)
			err = (xcb_generic_error_t*)event;
			handle_error("main/switch");
			break;
		}
		case XCB_PROPERTY_NOTIFY: {
			xcb_property_notify_event_t* pn_event = (xcb_property_notify_event_t*)event;
			if (pn_event->atom == atom_NET_CLIENT_LIST && pn_event->window == root_wid) {
				RAISE_ERROR(root_list_change());
			}
			if (pn_event->atom == atom_NET_WM_DESKTOP) {
				Child* entry = table_get(&child_table, pn_event->window);
				if (entry)
					RAISE_ERROR(geometry_get(pn_event->window, &entry->position));
			}
			break;
		}
		case XCB_CONFIGURE_NOTIFY: {
			xcb_configure_notify_event_t* cn_event = (xcb_configure_notify_event_t*)event;
			Child* entry = table_get(&child_table, cn_event->window);
			if (entry)
				RAISE_ERROR(geometry_get(cn_event->window, &entry->position));
			break;
		}
		case XCB_DESTROY_NOTIFY: {
			xcb_destroy_notify_event_t* dn_event = (xcb_destroy_notify_event_t*)event;
			Child* entry = table_del(&child_table, dn_event->window);
			if (entry) {
				printf("Closing child #%d\n", entry->parent->children);
				if (!--entry->parent->children) {
					printf("Closing parent 0x%x\n", entry->parent->wid);
					// Focus parent window
					xcb_client_message_event_t event = { 0 };
					event.response_type = XCB_CLIENT_MESSAGE;
					event.format = 32;
					event.window = entry->parent->wid;
					event.type = atom_NET_ACTIVE_WINDOW;
					event.data.data32[0] = 2;
					xcb_send_event(connection, 0, root_wid, XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY | XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT, (char*)&event);
					// Show parent
					xcb_map_window(connection, entry->parent->wid);
					// xcb_flush happens in geometry_set
					RAISE_ERROR(geometry_set(entry->parent->wid, &entry->position));
					free(table_del(&parent_table, entry->parent->pid));
				}
				free(entry);
			}
			break;
		}
		default: break;
		}
		free(event);
	}
}

// Environment variables:
// $XSWALLOW_IMMUNE, multiple values (:-seperated) for immune_pnames
// $XSWALLOW_TERMINALS, multiple values (:-seperated) for terminal_pnames
// $TERMINAL, single value for terminal_pnames
int pname_list_init() {
	// Count size of total array
	size_t obj_count = 0;
	char* env = getenv("XSWALLOW_IMMUNE");
	if (env && *env) {
		++obj_count;
		while (*env)
			obj_count += *env++ == ':';
	}
	env = getenv("XSWALLOW_TERMINALS");
	if (env && *env) {
		++obj_count;
		while (*env)
			obj_count += *env++ == ':';
	}
	if (getenv("TERMINAL"))
		++obj_count;
	char** objects = malloc((obj_count + 1) * sizeof(char*));
	if (!objects)
		ERRM_ALLOC(objects);
	immune_pnames = objects;
	env = getenv("XSWALLOW_IMMUNE");
	if (env && *env) {
		*objects++ = strtok(strdup(env), ":");
		while ((*objects = strtok(NULL, ":")))
			++objects;
	}
	terminal_pnames = objects;
	env = getenv("XSWALLOW_TERMINALS");
	if (env && *env) {
		*objects++ = strtok(strdup(env), ":");
		while ((*objects = strtok(NULL, ":")))
			++objects;
	}
	env = getenv("TERMINAL");
	if (env)
		*objects++ = "alacritty";
	*objects = NULL;
	return 0;
}

int pname_list_match(char** list, char* pname) {
	while (*list)
		if (!strcmp(*list++, pname))
			return 1;
	return 0;
}

// Linux-specific methods to get process info, feel free to contribute for other systems with appropriate #ifdef's
char* get_pname(pid_t pid) {
	// /proc/4294967296/comm
	char file_name[22];
	sprintf(file_name, "/proc/%"PRIu32"/comm", pid);
	FILE* handle = fopen(file_name, "r");
	// Turns out procfs doesn't bother to implement file sizes, so instead here's a growable buffer
	// I think comm is limited to 15 bytes, so start a bit above there (to allow newlines)
	// Also filenames can contain newlines, so only stop reading once EOF is reached
	size_t size = 17;
	size_t cursor = 0;
	// Extra byte for null
	char* buffer = malloc(size * sizeof(char));
	if (!buffer)
		return NULL;
	for (;;) {
		size_t read = fread(buffer + cursor, sizeof(char), size - cursor, handle);
		if (!read) {
			if (cursor > 0 && buffer[cursor - 1] == '\n')
				--cursor;
			buffer[cursor] = 0;
			break;
		}
		cursor += read;
		if (cursor == size) {
			size *= 2;
			if (!(buffer = realloc(buffer, size * sizeof(char))))
				return NULL;
		}
	}
	fclose(handle);
	return buffer;
}

pid_t get_ppid(pid_t pid) {
	// /proc/4294967296/status
	char file_name[24];
	sprintf(file_name, "/proc/%"PRIu32"/status", pid);
	FILE* handle = fopen(file_name, "r");
	// Since this one requires reading a larger file, I'm just going to give up and use a fixed-size buffer.
	// The PPid: line shouldn't be longer than 20 bytes anyways, but this'll make getting to it faster
	char buffer[256];
	pid_t out = 0;
	while (fgets(buffer, 256, handle))
		// There is a /tiny/ chance this accidentally triggers in the middle of another line,
		// it'll probably never happen but if you're wondering why it's suddenly crashing this might be it
		if (!strncmp(buffer, "PPid:\t", 6)) {
			sscanf(buffer, "PPid:\t%u", &out);
			break;
		}
	fclose(handle);
	return out;
}

int geometry_get(xcb_window_t wid, Geometry* out) {
	xcb_translate_coordinates_cookie_t pos_cookie = xcb_translate_coordinates(connection, wid, root_wid, 0, 0);
	xcb_get_geometry_cookie_t size_cookie = xcb_get_geometry(connection, wid);
	xcb_get_property_cookie_t desktop_cookie = xcb_get_property(connection, 0, wid, atom_NET_WM_DESKTOP, XCB_ATOM_CARDINAL, 0, 4);
	xcb_flush(connection);
	xcb_translate_coordinates_reply_t* pos_reply = xcb_translate_coordinates_reply(connection, pos_cookie, &err);
	HANDLE_ERROR("geometry_get/pos");
	xcb_get_geometry_reply_t* size_reply = xcb_get_geometry_reply(connection, size_cookie, &err);
	HANDLE_ERROR("geometry_get/size");
	out->x = pos_reply->dst_x - size_reply->x;
	out->y = pos_reply->dst_y - size_reply->y;
	out->w = size_reply->width;
	out->h = size_reply->height;
	free(pos_reply);
	free(size_reply);
	xcb_get_property_reply_t* desktop_reply = xcb_get_property_reply(connection, desktop_cookie, &err);
	HANDLE_ERROR("geometry_get/desktop");
	out->d = xcb_get_property_value_length(desktop_reply) ? *(uint32_t*)xcb_get_property_value(desktop_reply) : 0;
	free(desktop_reply);
	return 0;
}

int geometry_set(xcb_window_t wid, Geometry* in) {
	const uint32_t values[] = { in->x, in->y, in->w, in->h };
	xcb_configure_window(connection, wid, XCB_CONFIG_WINDOW_X | XCB_CONFIG_WINDOW_Y | XCB_CONFIG_WINDOW_WIDTH | XCB_CONFIG_WINDOW_HEIGHT, values);
	// Set window's desktop
	xcb_client_message_event_t event = { 0 };
	event.response_type = XCB_CLIENT_MESSAGE;
	event.format = 32;
	event.window = wid;
	event.type = atom_NET_WM_DESKTOP;
	event.data.data32[0] = in->d;
	event.data.data32[1] = 2;
	xcb_send_event(connection, 0, root_wid, XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY | XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT, (char*)&event);
	xcb_flush(connection);
	return 0;
}

int handle_new_window(xcb_window_t wid) {
	printf("New window: 0x%x\n", wid);
	xcb_get_property_cookie_t cookie = xcb_get_property(connection, 0, wid, atom_NET_WM_PID, XCB_ATOM_CARDINAL, 0, 4);
	xcb_flush(connection);
	xcb_get_property_reply_t* reply = xcb_get_property_reply(connection, cookie, &err);
	HANDLE_ERROR("new_window/cpid");
	// Skip window if it has no _NET_WM_PID
	if (!xcb_get_property_value_length(reply)) {
		free(reply);
		return 0;
	}
	// PID's might be larger than a u32, but that's all that X cardinals can hold
	pid_t pid = (pid_t)*(uint32_t*)xcb_get_property_value(reply);
	free(reply);
	printf("  %"PRIu32"", pid);
	char* pname = get_pname(pid);
	if (pname) {
		printf(" %s", pname);
		int match = pname_list_match(immune_pnames, pname);
		free(pname);
		if (match) {
			printf("\n  Process is immune\n");
			return 0;
		}
	}
	printf("\n");
	while ((pid = get_ppid(pid))) {
		printf("  %d", pid);
		char* pname = get_pname(pid);
		if (pname) {
			printf(" %s", pname);
			int match = pname_list_match(terminal_pnames, pname);
			free(pname);
			if (match) {
				printf("\n  Match located\n");
				Parent* parent_entry = table_get(&parent_table, pid);
				Child* child_entry = malloc(sizeof(Child));
				if (parent_entry) {
					++parent_entry->children;
					// get child position
					Geometry position;
					RAISE_ERROR(geometry_get(wid, &position));
					child_entry->position = position;
				} else {
					parent_entry = malloc(sizeof(Parent));
					parent_entry->children = 1;
					parent_entry->pid = pid;
					table_add(&parent_table, pid, parent_entry);
					// Find the parent window id, as a small optimization only check windows from window_list
					xcb_window_t parent_wid;
					for (int i = 0; i < window_list.length; ++i) {
						// root_list_change is still throwing around the ids
						if (!window_list.buffer[i])
							continue;
						xcb_get_property_cookie_t cookie = xcb_get_property(connection, 0, window_list.buffer[i], atom_NET_WM_PID, XCB_ATOM_CARDINAL, 0, 4);
						xcb_flush(connection);
						xcb_get_property_reply_t* reply = xcb_get_property_reply(connection, cookie, &err);
						HANDLE_ERROR("new_window/ppid");
						if (!xcb_get_property_value_length(reply)) {
							free(reply);
							continue;
						}
						pid_t ppid = (pid_t)*(uint32_t*)xcb_get_property_value(reply);
						free(reply);
						if (ppid == pid) {
							parent_wid = window_list.buffer[i];
							goto success;
						}
					}
					ERRM_NWIN;
					success:
					printf("  Parent window is 0x%x\n", parent_wid);
					parent_entry->wid = parent_wid;
					// Hide parent
					xcb_unmap_window(connection, parent_wid);
					Geometry position;
					RAISE_ERROR(geometry_get(parent_wid, &position));
					RAISE_ERROR(geometry_set(wid, &position));
					child_entry->position = position;
				}
				child_entry->parent = parent_entry;
				table_add(&child_table, wid, child_entry);
				// subscribe to child
				const uint32_t values[] = { XCB_EVENT_MASK_PROPERTY_CHANGE | XCB_EVENT_MASK_STRUCTURE_NOTIFY };
				xcb_change_window_attributes(connection, wid, XCB_CW_EVENT_MASK, values);
				xcb_flush(connection);
				return 0;
			}
		}
		printf("\n");
	}
	return 0;
}

int root_list_init() {
	// The goal here is to get as much data as is in the property,
	// length=-1 sounds awful, but I guess it works?
	xcb_get_property_cookie_t cookie = xcb_get_property(connection, 0, root_wid, atom_NET_CLIENT_LIST, XCB_ATOM_WINDOW, 0, -1);
	xcb_flush(connection);
	xcb_get_property_reply_t* reply = xcb_get_property_reply(connection, cookie, &err);
	HANDLE_ERROR("root_list_init/list");
	xcb_window_t* new_buffer = (xcb_window_t*)xcb_get_property_value(reply);
	int new_bytes = xcb_get_property_value_length(reply);
	memcpy(window_list.buffer = malloc(new_bytes), new_buffer, new_bytes);
	window_list.capacity = window_list.length = new_bytes / sizeof(xcb_window_t);
	return 0;
}

int root_list_change() {
// Same as above
	xcb_get_property_cookie_t cookie = xcb_get_property(connection, 0, root_wid, atom_NET_CLIENT_LIST, XCB_ATOM_WINDOW, 0, -1);
	xcb_flush(connection);
	xcb_get_property_reply_t* reply = xcb_get_property_reply(connection, cookie, &err);
	HANDLE_ERROR("root_list_change/list");
	xcb_window_t* new_buffer = (xcb_window_t*)xcb_get_property_value(reply);
	int new_length = xcb_get_property_value_length(reply) / sizeof(xcb_window_t);
	// _NET_CLIENT_LIST is fairly consistently ordered, so I can do an extremely linear scan
	// Falls back to a O(n²) search if that fails
	// Macros to make code look more consistent
	#define old_buffer window_list.buffer
	#define old_length window_list.length
	#define old_capacity window_list.capacity
	// In the worst case scenario, every old window is gone and every new window is added, account for that in the buffer capacity
	int new_capacity = (old_length + new_length);
	// Swap out length so that handle_new_window can see to the end if it needs
	int real_length = old_length;
	old_length = new_capacity;
	if (old_capacity < new_capacity)
		old_buffer = realloc(old_buffer, (old_capacity = new_capacity) * sizeof(xcb_window_t));
	if (old_buffer == NULL)
		ERRM_ALLOC(old_buffer);
	// Pre-fill new length with null to make this horrible mess work
	for (int i = old_length; i < new_length; ++i)
		old_buffer[i] = XCB_NONE;
	// Iterate through new items
	for (int i = 0; i < new_length; ++i) {
		xcb_window_t new_window = new_buffer[i];
		xcb_window_t old_window = old_buffer[i];
		// Fast case if both arrays match, just continue
		if (new_window == old_window)
			continue;
		// Try to find the match somewhere else
		for (int j = i + 1; j < real_length; ++j) {
			if (new_window == old_buffer[j]) {
				// Match!  Swap the old entry here
				old_buffer[j] = old_window;
				goto success;
			}
		}
		// This window is new, move the old entry to the end of the list
		old_buffer[real_length++] = old_window;
		RAISE_ERROR(handle_new_window(new_window));
		success:
		old_buffer[i] = new_window;
	}
	// Any closed windows have been moved somewhere to the end of the buffer, so they can be truncated off
	old_length = new_length;
	#undef old_buffer
	#undef old_length
	#undef old_capacity
	free(reply);
	return 0;
}
