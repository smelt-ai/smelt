// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'api.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$AcpEntry {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is AcpEntry);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'AcpEntry()';
}


}

/// @nodoc
class $AcpEntryCopyWith<$Res>  {
$AcpEntryCopyWith(AcpEntry _, $Res Function(AcpEntry) __);
}


/// Adds pattern-matching-related methods to [AcpEntry].
extension AcpEntryPatterns on AcpEntry {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( AcpEntry_User value)?  user,TResult Function( AcpEntry_Assistant value)?  assistant,TResult Function( AcpEntry_ToolCall value)?  toolCall,required TResult orElse(),}){
final _that = this;
switch (_that) {
case AcpEntry_User() when user != null:
return user(_that);case AcpEntry_Assistant() when assistant != null:
return assistant(_that);case AcpEntry_ToolCall() when toolCall != null:
return toolCall(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( AcpEntry_User value)  user,required TResult Function( AcpEntry_Assistant value)  assistant,required TResult Function( AcpEntry_ToolCall value)  toolCall,}){
final _that = this;
switch (_that) {
case AcpEntry_User():
return user(_that);case AcpEntry_Assistant():
return assistant(_that);case AcpEntry_ToolCall():
return toolCall(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( AcpEntry_User value)?  user,TResult? Function( AcpEntry_Assistant value)?  assistant,TResult? Function( AcpEntry_ToolCall value)?  toolCall,}){
final _that = this;
switch (_that) {
case AcpEntry_User() when user != null:
return user(_that);case AcpEntry_Assistant() when assistant != null:
return assistant(_that);case AcpEntry_ToolCall() when toolCall != null:
return toolCall(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( String text)?  user,TResult Function( String text,  bool thought)?  assistant,TResult Function( String id,  String title,  ToolKind kind,  ToolStatus status,  List<String> output)?  toolCall,required TResult orElse(),}) {final _that = this;
switch (_that) {
case AcpEntry_User() when user != null:
return user(_that.text);case AcpEntry_Assistant() when assistant != null:
return assistant(_that.text,_that.thought);case AcpEntry_ToolCall() when toolCall != null:
return toolCall(_that.id,_that.title,_that.kind,_that.status,_that.output);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( String text)  user,required TResult Function( String text,  bool thought)  assistant,required TResult Function( String id,  String title,  ToolKind kind,  ToolStatus status,  List<String> output)  toolCall,}) {final _that = this;
switch (_that) {
case AcpEntry_User():
return user(_that.text);case AcpEntry_Assistant():
return assistant(_that.text,_that.thought);case AcpEntry_ToolCall():
return toolCall(_that.id,_that.title,_that.kind,_that.status,_that.output);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( String text)?  user,TResult? Function( String text,  bool thought)?  assistant,TResult? Function( String id,  String title,  ToolKind kind,  ToolStatus status,  List<String> output)?  toolCall,}) {final _that = this;
switch (_that) {
case AcpEntry_User() when user != null:
return user(_that.text);case AcpEntry_Assistant() when assistant != null:
return assistant(_that.text,_that.thought);case AcpEntry_ToolCall() when toolCall != null:
return toolCall(_that.id,_that.title,_that.kind,_that.status,_that.output);case _:
  return null;

}
}

}

/// @nodoc


class AcpEntry_User extends AcpEntry {
  const AcpEntry_User({required this.text}): super._();
  

 final  String text;

/// Create a copy of AcpEntry
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$AcpEntry_UserCopyWith<AcpEntry_User> get copyWith => _$AcpEntry_UserCopyWithImpl<AcpEntry_User>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is AcpEntry_User&&(identical(other.text, text) || other.text == text));
}


@override
int get hashCode => Object.hash(runtimeType,text);

@override
String toString() {
  return 'AcpEntry.user(text: $text)';
}


}

/// @nodoc
abstract mixin class $AcpEntry_UserCopyWith<$Res> implements $AcpEntryCopyWith<$Res> {
  factory $AcpEntry_UserCopyWith(AcpEntry_User value, $Res Function(AcpEntry_User) _then) = _$AcpEntry_UserCopyWithImpl;
@useResult
$Res call({
 String text
});




}
/// @nodoc
class _$AcpEntry_UserCopyWithImpl<$Res>
    implements $AcpEntry_UserCopyWith<$Res> {
  _$AcpEntry_UserCopyWithImpl(this._self, this._then);

  final AcpEntry_User _self;
  final $Res Function(AcpEntry_User) _then;

/// Create a copy of AcpEntry
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? text = null,}) {
  return _then(AcpEntry_User(
text: null == text ? _self.text : text // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class AcpEntry_Assistant extends AcpEntry {
  const AcpEntry_Assistant({required this.text, required this.thought}): super._();
  

 final  String text;
 final  bool thought;

/// Create a copy of AcpEntry
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$AcpEntry_AssistantCopyWith<AcpEntry_Assistant> get copyWith => _$AcpEntry_AssistantCopyWithImpl<AcpEntry_Assistant>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is AcpEntry_Assistant&&(identical(other.text, text) || other.text == text)&&(identical(other.thought, thought) || other.thought == thought));
}


@override
int get hashCode => Object.hash(runtimeType,text,thought);

@override
String toString() {
  return 'AcpEntry.assistant(text: $text, thought: $thought)';
}


}

/// @nodoc
abstract mixin class $AcpEntry_AssistantCopyWith<$Res> implements $AcpEntryCopyWith<$Res> {
  factory $AcpEntry_AssistantCopyWith(AcpEntry_Assistant value, $Res Function(AcpEntry_Assistant) _then) = _$AcpEntry_AssistantCopyWithImpl;
@useResult
$Res call({
 String text, bool thought
});




}
/// @nodoc
class _$AcpEntry_AssistantCopyWithImpl<$Res>
    implements $AcpEntry_AssistantCopyWith<$Res> {
  _$AcpEntry_AssistantCopyWithImpl(this._self, this._then);

  final AcpEntry_Assistant _self;
  final $Res Function(AcpEntry_Assistant) _then;

/// Create a copy of AcpEntry
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? text = null,Object? thought = null,}) {
  return _then(AcpEntry_Assistant(
text: null == text ? _self.text : text // ignore: cast_nullable_to_non_nullable
as String,thought: null == thought ? _self.thought : thought // ignore: cast_nullable_to_non_nullable
as bool,
  ));
}


}

/// @nodoc


class AcpEntry_ToolCall extends AcpEntry {
  const AcpEntry_ToolCall({required this.id, required this.title, required this.kind, required this.status, required final  List<String> output}): _output = output,super._();
  

 final  String id;
 final  String title;
 final  ToolKind kind;
 final  ToolStatus status;
 final  List<String> _output;
 List<String> get output {
  if (_output is EqualUnmodifiableListView) return _output;
  // ignore: implicit_dynamic_type
  return EqualUnmodifiableListView(_output);
}


/// Create a copy of AcpEntry
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$AcpEntry_ToolCallCopyWith<AcpEntry_ToolCall> get copyWith => _$AcpEntry_ToolCallCopyWithImpl<AcpEntry_ToolCall>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is AcpEntry_ToolCall&&(identical(other.id, id) || other.id == id)&&(identical(other.title, title) || other.title == title)&&(identical(other.kind, kind) || other.kind == kind)&&(identical(other.status, status) || other.status == status)&&const DeepCollectionEquality().equals(other._output, _output));
}


@override
int get hashCode => Object.hash(runtimeType,id,title,kind,status,const DeepCollectionEquality().hash(_output));

@override
String toString() {
  return 'AcpEntry.toolCall(id: $id, title: $title, kind: $kind, status: $status, output: $output)';
}


}

/// @nodoc
abstract mixin class $AcpEntry_ToolCallCopyWith<$Res> implements $AcpEntryCopyWith<$Res> {
  factory $AcpEntry_ToolCallCopyWith(AcpEntry_ToolCall value, $Res Function(AcpEntry_ToolCall) _then) = _$AcpEntry_ToolCallCopyWithImpl;
@useResult
$Res call({
 String id, String title, ToolKind kind, ToolStatus status, List<String> output
});




}
/// @nodoc
class _$AcpEntry_ToolCallCopyWithImpl<$Res>
    implements $AcpEntry_ToolCallCopyWith<$Res> {
  _$AcpEntry_ToolCallCopyWithImpl(this._self, this._then);

  final AcpEntry_ToolCall _self;
  final $Res Function(AcpEntry_ToolCall) _then;

/// Create a copy of AcpEntry
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? id = null,Object? title = null,Object? kind = null,Object? status = null,Object? output = null,}) {
  return _then(AcpEntry_ToolCall(
id: null == id ? _self.id : id // ignore: cast_nullable_to_non_nullable
as String,title: null == title ? _self.title : title // ignore: cast_nullable_to_non_nullable
as String,kind: null == kind ? _self.kind : kind // ignore: cast_nullable_to_non_nullable
as ToolKind,status: null == status ? _self.status : status // ignore: cast_nullable_to_non_nullable
as ToolStatus,output: null == output ? _self._output : output // ignore: cast_nullable_to_non_nullable
as List<String>,
  ));
}


}

// dart format on
