; survol index query for Kotlin (tree-sitter-kotlin-ng).
; Same capture conventions as java.scm. When several patterns capture the same
; node, the first pattern in this file wins (specific kinds come first).
; Names captured on type nodes are normalised in code (`a.b.C<T>?` -> `C`).

(package_header (qualified_identifier) @package)
(import) @import

; ---- definitions
(class_declaration "interface" name: (identifier) @name) @definition.interface
(class_declaration (modifiers (class_modifier "enum")) name: (identifier) @name) @definition.enum
(class_declaration (modifiers (class_modifier "annotation")) name: (identifier) @name) @definition.annotation
(class_declaration name: (identifier) @name) @definition.class
(object_declaration name: (identifier) @name) @definition.object
(companion_object name: (identifier)? @name) @definition.object
(function_declaration name: (identifier) @name) @definition.function
(secondary_constructor) @definition.constructor
(class_body (property_declaration (variable_declaration . (identifier) @name)) @definition.field)
(enum_entry . (identifier) @name) @definition.field

; ---- references
(call_expression . [(identifier) @name
                    (navigation_expression . (_) @receiver (identifier) @name .)]) @reference.call
(callable_reference (_)? @receiver . (identifier) @name .) @reference.call
(delegation_specifier (constructor_invocation (user_type) @name)) @reference.extends
(delegation_specifier (user_type) @name) @reference.implements
(delegation_specifier (explicit_delegation (user_type) @name)) @reference.implements
(user_type) @name @reference.type

; ---- typed bindings
(class_parameter (identifier) @binding.name [(user_type) (nullable_type)] @binding.type) @binding.field
(parameter . (identifier) @binding.name [(user_type) (nullable_type)] @binding.type) @binding.local
(property_declaration (variable_declaration . (identifier) @binding.name [(user_type) (nullable_type)] @binding.type)) @binding.local
(property_declaration (variable_declaration . (identifier) @binding.name .) (call_expression . (identifier) @binding.type)) @binding.local

; ---- values: initialisers of constants, fields and locals (string-like ones
; are kept in code: URLs, config keys, environment objects)
(property_declaration (variable_declaration . (identifier) @value.name) "=" . (_) @value.expr) @value
