from setuptools import setup

setup(
    name='proxy-ssh',
    version='1.0.0',
    description='SSH 反向代理隧道工具 - 支持自动重连和心跳保活',
    author='User',
    py_modules=['proxy_ssh'],
    python_requires='>=3.7',
    install_requires=[
        'rich>=13.0.0',
    ],
    entry_points={
        'console_scripts': [
            'pss=proxy_ssh:main',
            'proxy-ssh=proxy_ssh:main',
        ],
    },
    classifiers=[
        'Development Status :: 4 - Beta',
        'Environment :: Console',
        'Intended Audience :: Developers',
        'Intended Audience :: System Administrators',
        'License :: OSI Approved :: MIT License',
        'Operating System :: OS Independent',
        'Programming Language :: Python :: 3',
        'Programming Language :: Python :: 3.7',
        'Programming Language :: Python :: 3.8',
        'Programming Language :: Python :: 3.9',
        'Programming Language :: Python :: 3.10',
        'Programming Language :: Python :: 3.11',
        'Programming Language :: Python :: 3.12',
        'Topic :: System :: Networking',
        'Topic :: Utilities',
    ],
)
