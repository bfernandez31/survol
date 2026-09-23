; survol index query for Java.
; Captures: @definition.<kind> + @name, @reference.<kind> + @name (+ @receiver),
; @import, @package, @binding.<field|local> + @binding.name + @binding.type,
; @value + @value.name + @value.expr.
; Annotations, containers, arity and parameters are computed in code.

(package_declaration [(identifier) (scoped_identifier)] @package)
(import_declaration) @import

; ---- definitions
(class_declaration name: (identifier) @name) @definition.class
(interface_declaration name: (identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(record_declaration name: (identifier) @name) @definition.record
(annotation_type_declaration name: (identifier) @name) @definition.annotation
(method_declaration name: (identifier) @name) @definition.method
(annotation_type_element_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.constructor
(compact_constructor_declaration name: (identifier) @name) @definition.constructor
(field_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.field
(enum_constant name: (identifier) @name) @definition.field

; ---- references
(method_invocation object: (_)? @receiver name: (identifier) @name) @reference.call
(method_reference . (_) @receiver (identifier) @name .) @reference.call
(object_creation_expression type: [(type_identifier) @name
                                   (generic_type (type_identifier) @name)
                                   (scoped_type_identifier (type_identifier) @name .)]) @reference.new
(superclass [(type_identifier) @name
             (generic_type (type_identifier) @name)
             (scoped_type_identifier (type_identifier) @name .)]) @reference.extends
(super_interfaces (type_list [(type_identifier) @name
                              (generic_type (type_identifier) @name)
                              (scoped_type_identifier (type_identifier) @name .)])) @reference.implements
(extends_interfaces (type_list [(type_identifier) @name
                                (generic_type (type_identifier) @name)
                                (scoped_type_identifier (type_identifier) @name .)])) @reference.extends
(type_identifier) @name @reference.type
(class_literal (type_identifier) @name) @reference.type

; ---- typed bindings, used to resolve receivers
(field_declaration type: (_) @binding.type
  declarator: (variable_declarator name: (identifier) @binding.name)) @binding.field
(formal_parameter type: (_) @binding.type name: (identifier) @binding.name) @binding.local
(spread_parameter [(type_identifier) (generic_type) (scoped_type_identifier)] @binding.type (variable_declarator name: (identifier) @binding.name)) @binding.local
(local_variable_declaration type: (_) @binding.type
  declarator: (variable_declarator name: (identifier) @binding.name)) @binding.local
(enhanced_for_statement type: (_) @binding.type name: (identifier) @binding.name) @binding.local
(catch_formal_parameter (catch_type (_) @binding.type) name: (identifier) @binding.name) @binding.local
(local_variable_declaration
  declarator: (variable_declarator name: (identifier) @binding.name
    value: (object_creation_expression type: (_) @binding.type))) @binding.local
(record_declaration parameters: (formal_parameters
  (formal_parameter type: (_) @binding.type name: (identifier) @binding.name) @binding.field))

; ---- values: initialisers of constants, fields and locals (string-like ones
; are kept in code: URLs, config keys, environment objects)
(variable_declarator name: (identifier) @value.name value: (_) @value.expr) @value
