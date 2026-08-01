import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/acp_snapshot.dart';
import 'package:smelt_mobile/services/message_draft_store.dart';

void main() {
  test('persists, updates, and deletes a session draft', () async {
    final directory = await Directory.systemTemp.createTemp(
      'smelt-message-drafts-',
    );
    addTearDown(() => directory.delete(recursive: true));
    final store = FileMessageDraftStore(
      directoryProvider: () async => directory,
    );

    await store.save(
      'session/with unsafe characters',
      const MessageDraft(
        content: 'Check this image',
        images: [AcpImageData(mimeType: 'image/png', base64: 'aW1hZ2U=')],
        requestId: 'request-1',
      ),
    );

    final pending = await store.load('session/with unsafe characters');
    expect(pending?.content, 'Check this image');
    expect(pending?.images.single.mimeType, 'image/png');
    expect(pending?.requestId, 'request-1');

    await store.save(
      'session/with unsafe characters',
      pending!.copyWith(clearRequestId: true),
    );
    expect(
      (await store.load('session/with unsafe characters'))?.requestId,
      isNull,
    );

    await store.delete('session/with unsafe characters');
    expect(await store.load('session/with unsafe characters'), isNull);
  });

  test('serializes overlapping writes for the same session', () async {
    final directory = await Directory.systemTemp.createTemp(
      'smelt-message-drafts-',
    );
    addTearDown(() => directory.delete(recursive: true));
    final store = FileMessageDraftStore(
      directoryProvider: () async => directory,
    );

    await Future.wait([
      store.save('session-1', const MessageDraft(content: 'first')),
      store.save('session-1', const MessageDraft(content: 'second')),
    ]);

    expect((await store.load('session-1'))?.content, 'second');
  });

  test('a stale acknowledgement cannot delete a newer draft', () async {
    final directory = await Directory.systemTemp.createTemp(
      'smelt-message-drafts-',
    );
    addTearDown(() => directory.delete(recursive: true));
    final store = FileMessageDraftStore(
      directoryProvider: () async => directory,
    );

    await store.save(
      'session-1',
      const MessageDraft(content: 'sent message', requestId: 'old-request'),
    );
    await store.save('session-1', const MessageDraft(content: 'new draft'));

    expect(
      await store.resolveRequest('session-1', 'old-request', succeeded: true),
      isFalse,
    );
    expect((await store.load('session-1'))?.content, 'new draft');
  });

  test('a matching acknowledgement resolves only its own draft', () async {
    final directory = await Directory.systemTemp.createTemp(
      'smelt-message-drafts-',
    );
    addTearDown(() => directory.delete(recursive: true));
    final store = FileMessageDraftStore(
      directoryProvider: () async => directory,
    );

    await store.save(
      'session-1',
      const MessageDraft(content: 'retry me', requestId: 'request-1'),
    );
    expect(
      await store.resolveRequest('session-1', 'request-1', succeeded: false),
      isTrue,
    );
    final retry = await store.load('session-1');
    expect(retry?.content, 'retry me');
    expect(retry?.requestId, isNull);

    await store.save(
      'session-1',
      const MessageDraft(content: 'sent', requestId: 'request-2'),
    );
    expect(
      await store.resolveRequest('session-1', 'request-2', succeeded: true),
      isTrue,
    );
    expect(await store.load('session-1'), isNull);
  });
}
