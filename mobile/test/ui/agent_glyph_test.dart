import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/ui/agent_glyph.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('Oh My Pi has its own bundled icon and display name', () async {
    resetAgentIconsForTest();
    await loadAgentIcons();
    expect(agentIconAsset('omp'), 'assets/agent-icons/agent-omp.svg');
    expect(agentIconAsset('omp-work'), 'assets/agent-icons/agent-omp.svg');
    expect(agentIconAsset('pi'), 'assets/agent-icons/agent-pi.svg');
    expect(agentDisplayName('omp'), 'Oh My Pi');
    expect(agentDisplayName('omp-work'), 'Oh My Pi');
    expect(agentDisplayName('pi'), 'Pi');
    expect(agentIconAsset('company'), isNull);
    expect(agentDisplayName('company'), 'Company');
  });
}
